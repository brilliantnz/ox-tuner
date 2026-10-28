//! The M7 gate against the simulator: guardrails S1–S10, a verified write and a
//! verified burn.
//!
//! Every test drives the production command path. The simulator is the virtual
//! ECU from `oxtuner-sim`, so a partial write, a refused burn and a link loss
//! are real wire behaviour, not mocks.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use oxtuner_core::{
    BlockingEventStream, CoreConfig, CoreHandle, CoreThread, Event, FailureCode, JournalEntry,
    OpId, PortChoice, ProjectSummary, SimConfig, SimOp, start,
};
use oxtuner_project::TUNE_FILE;
use oxtuner_sim::Fault;

const WAIT: Duration = Duration::from_secs(10);

const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"
queryCommand = "Q"

[Constants]
endianness = little
nPages = 1
pageSize = 16
page = 1
pageIdentifier = "\x00\x01"
pageReadCommand = "p%2i%2o%2c"
pageValueWrite = "M%2i%2o%2c%v"
burnCommand = "b%2i"
blockingFactor = 16
pageActivationDelay = 1
reqFuel = scalar, U08, 12, "ms", 0.1, 0, 0, 25.5, 1
veTable = array, U08, 0, [2x2], "%", 1, 0, 0, 255, 0
rpmBins = array, U16, 4, [2], "rpm", 1, 0, 0, 10000, 0
mapBins = array, U08, 8, [2], "kPa", 1, 0, 0, 255, 0

[TableEditor]
table = veTableTbl, veMap, "VE Table", 1
  xBins = rpmBins, rpm
  yBins = mapBins, map
  zBins = veTable
  xyLabels = "RPM", "MAP"

[OutputChannels]
ochGetCommand = "r%2o%2c"
ochBlockSize = 4
rpm = scalar, U16, 0, "rpm", 1.0, 0.0
map = scalar, U08, 2, "kPa", 1.0, 0.0

[UserDefined]
dialog = engineDialog, "Engine"
  field = "Req. Fuel", reqFuel
"#;

const TUNE: &str = r#"<?xml version="1.0" encoding="ISO-8859-1"?>
<msq xmlns="http://www.msefi.com/:msq">
    <bibliography author="TunerStudio" tuneComment="fixture"/>
    <versionInfo fileFormat="5.0" firmwareInfo="Speeduino+2025.01.3" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="16">
        <constant cols="2" digits="0" name="veTable" rows="2" units="%">10.0 20.0 30.0 40.0</constant>
        <constant name="rpmBins">1000.0 2000.0</constant>
        <constant name="mapBins">50.0 100.0</constant>
        <constant name="reqFuel">12.0</constant>
    </page>
    <settings Comment="fixture"/>
</msq>
"#;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "oxtuner-core-writes-{label}-{}-{sequence}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(path.join("projectCfg")).expect("temporary project is creatable");
        fs::write(path.join("projectCfg/mainController.ini"), DEFINITION)
            .expect("the definition is writable");
        fs::write(path.join(TUNE_FILE), TUNE).expect("the tune is writable");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn journal(&self) -> PathBuf {
        self.path.join(".ox-tuner/write-journal.log")
    }

    fn snapshots(&self) -> PathBuf {
        self.path.join(".ox-tuner/snapshots")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct Harness {
    handle: CoreHandle,
    thread: CoreThread,
    events: BlockingEventStream,
}

impl Harness {
    fn start(writes_enabled: bool) -> Self {
        let parts = start(CoreConfig {
            writes_enabled,
            ..CoreConfig::default()
        })
        .expect("the core starts");
        Self {
            handle: parts.handle,
            thread: parts.thread,
            events: parts.events.into_blocking(),
        }
    }

    fn wait(&self, wanted: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(event) = self.events.recv_timeout(left) else {
                assert!(
                    !self.events.is_closed(),
                    "the core stopped before answering"
                );
                panic!("no matching event before the deadline");
            };
            if wanted(&event) {
                return event;
            }
        }
    }

    fn open(&self, path: &Path) -> ProjectSummary {
        let op = self.handle.open_project(path).expect("queued");
        match self.wait(|event| event.op() == Some(op)) {
            Event::ProjectOpened { summary, .. } => summary,
            other => panic!("expected a project open, got {other:?}"),
        }
    }

    fn connect(&self, config: SimConfig) -> oxtuner_core::ConnectionSummary {
        let op = self
            .handle
            .connect(PortChoice::Simulator(config), None)
            .expect("queued");
        match self.wait(|event| event.op() == Some(op)) {
            Event::Connected { summary, .. } => summary,
            other => panic!("expected a connection, got {other:?}"),
        }
    }

    fn read_all(&self) {
        let op = self.handle.read_all().expect("queued");
        match self.wait(|event| event.op() == Some(op)) {
            Event::PagesRead { summary, .. } => assert_eq!(summary.skipped, 0),
            other => panic!("expected a page read, got {other:?}"),
        }
    }

    fn edit_cell(&self, value: f64) {
        let op = self
            .handle
            .edit_table(
                "veTable",
                vec![oxtuner_core::CellEdit {
                    row: 0,
                    column: 0,
                    value,
                }],
            )
            .expect("queued");
        match self.wait(|event| event.op() == Some(op)) {
            Event::TableEdited { table, .. } => assert_eq!(table.cells[0], value),
            other => panic!("expected a table edit, got {other:?}"),
        }
    }

    fn write(&self) -> Event {
        let op = self.handle.write_to_ram(Vec::new()).expect("queued");
        self.wait(|event| event.op() == Some(op))
    }

    fn burn(&self) -> Event {
        let op = self.handle.burn_to_flash(Vec::new()).expect("queued");
        self.wait(|event| event.op() == Some(op))
    }

    fn journal(&self) -> (Option<PathBuf>, Vec<JournalEntry>) {
        let op = self.handle.read_journal().expect("queued");
        match self.wait(|event| event.op() == Some(op)) {
            Event::JournalRead { path, entries, .. } => (path, entries),
            other => panic!("expected a journal, got {other:?}"),
        }
    }

    fn table(&self) -> oxtuner_core::TableSummary {
        let op = self.handle.read_table("veTable").expect("queued");
        match self.wait(|event| event.op() == Some(op)) {
            Event::TableRead { table, .. } => table,
            other => panic!("expected a table, got {other:?}"),
        }
    }

    fn close(self) {
        self.thread.join();
    }
}

fn failure(event: &Event) -> (FailureCode, String) {
    match event {
        Event::Failed { failure, .. } => (failure.code, failure.message.clone()),
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[test]
fn a_write_round_trips_and_reads_back_verified() {
    let dir = TempDir::new("verified");
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect(SimConfig::default());
    core.read_all();
    core.edit_cell(77.0);

    let event = core.write();

    let Event::TuneWritten { summary, .. } = event else {
        panic!("expected a verified write, got {event:?}");
    };
    assert!(summary.verified);
    assert_eq!(summary.pages, 1);
    assert!(summary.cells >= 1);
    assert!(summary.variables >= 1);
    let written_cells = summary.cells;
    assert!(summary.snapshot.as_ref().is_some_and(|path| path.exists()));
    assert!(summary.journal.as_ref().is_some_and(|path| path.exists()));

    let table = core.table();
    assert_eq!(table.cells[0], 77.0);
    assert_eq!(
        table.states[0],
        oxtuner_core::CellState::InRam,
        "the verified write moved the cell into RAM state"
    );

    // The delta is now in RAM; a second write has nothing to send.
    let (code, message) = failure(&core.write());
    assert_eq!(code, FailureCode::WriteRefused);
    assert!(message.contains("no values that differ"), "{message}");

    let (path, entries) = core.journal();
    assert_eq!(path.as_deref(), Some(dir.journal().as_path()));
    assert_eq!(entries.len(), 2, "begin and result");
    assert_eq!(entries[0].phase, "begin");
    assert_eq!(entries[1].phase, "result");
    assert_eq!(entries[1].result, "ok");
    assert_eq!(entries[1].cells, written_cells);

    core.close();
}

#[test]
fn a_write_is_clamped_to_the_definition_range() {
    let dir = TempDir::new("clamp");
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect(SimConfig::default());
    core.read_all();

    let op = core
        .handle
        .edit_scalar("engineDialog", "reqFuel", 500.0)
        .expect("queued");
    let event = core.wait(|event| event.op() == Some(op));
    let Event::DialogEdited { dialog, .. } = event else {
        panic!("expected a dialog edit, got {event:?}");
    };
    let field = dialog
        .fields
        .iter()
        .find(|field| field.variable == "reqFuel")
        .expect("reqFuel is in the dialog");
    assert_eq!(field.value, Some(25.5), "clamped to the INI maximum (S2)");

    core.close();
}

#[test]
fn a_burn_follows_a_verified_write_and_is_journaled() {
    let dir = TempDir::new("burn");
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect(SimConfig::default());
    core.read_all();
    core.edit_cell(66.0);

    let event = core.write();
    assert!(matches!(event, Event::TuneWritten { .. }), "{event:?}");

    let event = core.burn();
    let Event::TuneBurned { summary, .. } = event else {
        panic!("expected a verified burn, got {event:?}");
    };
    assert!(summary.verified);
    assert!(summary.snapshot.as_ref().is_some_and(|path| path.exists()));

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
fn writes_are_off_by_default_and_the_switch_gates_them() {
    let dir = TempDir::new("off");
    let core = Harness::start(false);
    core.open(dir.path());
    core.connect(SimConfig::default());
    core.read_all();
    core.edit_cell(50.0);

    let (code, message) = failure(&core.write());
    assert_eq!(code, FailureCode::WriteRefused);
    assert!(message.contains("disabled"), "{message}");

    let op = core.handle.set_writes_enabled(true).expect("queued");
    let event = core.wait(|event| event.op() == Some(op));
    let Event::WriteAccessChanged {
        writes_enabled,
        connection,
        ..
    } = event
    else {
        panic!("expected a write access change, got {event:?}");
    };
    assert!(writes_enabled);
    assert!(connection.is_some());

    let event = core.write();
    assert!(matches!(event, Event::TuneWritten { .. }), "{event:?}");

    core.close();
}

#[test]
fn a_signature_mismatch_cannot_be_written_to() {
    let dir = TempDir::new("mismatch");
    let core = Harness::start(true);
    core.open(dir.path());
    let summary = core.connect(SimConfig {
        signature: Some("speeduino 999999".to_owned()),
        ..SimConfig::default()
    });
    assert!(summary.read_only);
    core.read_all();
    core.edit_cell(48.0);

    let (code, message) = failure(&core.write());
    assert_eq!(code, FailureCode::WriteRefused);
    assert!(message.contains("S1"), "{message}");

    core.close();
}

#[test]
fn a_partial_write_fails_verification_and_halts_the_path() {
    let dir = TempDir::new("partial");
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect(SimConfig {
        faults: vec![oxtuner_core::FaultRule::once(
            SimOp::WritePage,
            Fault::PartialWrite { bytes: 0 },
        )],
        ..SimConfig::default()
    });
    core.read_all();
    core.edit_cell(90.0);

    let event = core.write();
    let (code, message) = failure(&event);
    assert_eq!(code, FailureCode::WriteFailed, "{message}");
    assert!(message.contains("read-back"), "{message}");

    let table = core.table();
    assert_eq!(
        table.states[0],
        oxtuner_core::CellState::WriteFailed,
        "the changed cell is marked failed"
    );

    // The halt is a latch: a second write must not reach the wire even though
    // writes are still enabled.
    let (code, message) = failure(&core.write());
    assert_eq!(code, FailureCode::WriteRefused);
    assert!(message.contains("halted"), "{message}");

    let op = core.handle.acknowledge_write_failure().expect("queued");
    assert!(matches!(
        core.wait(|event| event.op() == Some(op)),
        Event::WriteFailureAcknowledged { .. }
    ));

    core.close();
}

#[test]
fn burning_before_writing_the_edit_is_refused() {
    let dir = TempDir::new("burn-order");
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect(SimConfig::default());
    core.read_all();
    core.edit_cell(55.0);

    let (code, message) = failure(&core.burn());
    assert_eq!(code, FailureCode::BurnRefused);
    assert!(message.contains("not in ECU RAM"), "{message}");

    // No burn reached the wire, so the journal has no burn entry.
    let (_, entries) = core.journal();
    assert!(entries.iter().all(|entry| entry.operation != "burn"));

    core.close();
}

#[test]
fn revert_snapshots_first_and_restores_the_baseline() {
    let dir = TempDir::new("revert");
    let core = Harness::start(true);
    core.open(dir.path());
    core.edit_cell(33.0);

    let op = core.handle.revert_tune().expect("queued");
    let event = core.wait(|event| event.op() == Some(op));
    let Event::TuneReverted { summary, .. } = event else {
        panic!("expected a revert, got {event:?}");
    };
    assert_eq!(summary.cells, 1);
    assert!(summary.snapshot.as_ref().is_some_and(|path| path.exists()));

    let table = core.table();
    assert_eq!(table.cells[0], 10.0, "back to the file baseline");

    let (_, entries) = core.journal();
    assert!(
        entries
            .iter()
            .any(|entry| entry.operation == "revert" && entry.result == "ok"),
        "a revert is journaled"
    );

    core.close();
}

#[test]
fn importing_a_different_signature_needs_consent() {
    let dir = TempDir::new("import");
    let other = dir.path().join("other.msq");
    fs::write(
        &other,
        TUNE.replace("signature=\"speeduino 202501\"", "signature=\"other 1\""),
    )
    .expect("other tune");
    let core = Harness::start(true);
    core.open(dir.path());

    let op = core.handle.import_tune(&other, false).expect("queued");
    let (code, message) = failure(&core.wait(|event| event.op() == Some(op)));
    assert_eq!(code, FailureCode::ImportNeedsConsent);
    assert!(message.contains("consent"), "{message}");

    let op = core.handle.import_tune(&other, true).expect("queued");
    let event = core.wait(|event| event.op() == Some(op));
    let Event::TuneImported { summary, .. } = event else {
        panic!("expected an import, got {event:?}");
    };
    assert!(summary.consented);
    assert!(summary.snapshot.as_ref().is_some_and(|path| path.exists()));

    let (_, entries) = core.journal();
    assert!(
        entries
            .iter()
            .any(|entry| entry.operation == "import" && entry.result == "ok"),
        "an import is journaled"
    );

    core.close();
}

#[test]
fn a_bad_import_is_reported_and_leaves_the_tune_alone() {
    let dir = TempDir::new("bad-import");
    let other = dir.path().join("broken.msq");
    fs::write(&other, b"not xml at all").expect("broken tune");
    let core = Harness::start(true);
    core.open(dir.path());

    let op = core.handle.import_tune(&other, true).expect("queued");
    let (code, _) = failure(&core.wait(|event| event.op() == Some(op)));
    assert_eq!(code, FailureCode::ImportRejected);

    let table = core.table();
    assert_eq!(table.cells[0], 10.0);

    core.close();
}

#[test]
fn no_edit_or_read_ever_burns() {
    let dir = TempDir::new("no-auto-burn");
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect(SimConfig::default());
    core.read_all();
    core.edit_cell(22.0);
    let _ = core.write();
    let _ = core.table();

    let (_, entries) = core.journal();
    assert!(entries.iter().all(|entry| entry.operation == "write"));
    assert!(
        entries.iter().all(|entry| entry.operation != "burn"),
        "nothing but an explicit burn command may burn (S5)"
    );

    core.close();
}

#[test]
fn the_poller_is_held_while_a_write_runs() {
    let dir = TempDir::new("poller-held");
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect(SimConfig {
        faults: vec![oxtuner_core::FaultRule::once(
            SimOp::WritePage,
            Fault::Delay {
                delay: Duration::from_millis(150),
            },
        )],
        ..SimConfig::default()
    });
    core.read_all();
    core.edit_cell(61.0);

    // `read_all` auto-starts polling; stop it and let an in-flight reply land so
    // the only frame this test could see belongs to the write's own window.
    let stop = core.handle.stop_realtime().expect("queued");
    let _ = core.wait(|event| event.op() == Some(stop));
    std::thread::sleep(Duration::from_millis(150));
    while core.events.try_recv().is_some() {}

    let write = core.handle.write_to_ram(Vec::new()).expect("queued");
    // The poll schedule starts while the write transaction is already active.
    // If the scheduler let a poll through, a frame would arrive before the
    // write's answer.
    let realtime = core.handle.start_realtime(10).expect("queued");
    let deadline = Instant::now() + WAIT;
    let mut saw_realtime = false;
    while Instant::now() < deadline {
        let Some(event) = core
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        else {
            panic!("the write never finished");
        };
        if event.op() == Some(realtime) {
            saw_realtime = true;
        }
        if event.op() == Some(write) {
            assert!(saw_realtime, "the poll start was answered");
            assert!(
                matches!(event, Event::TuneWritten { .. }),
                "the write should have succeeded: {event:?}"
            );
            core.close();
            return;
        }
        assert!(
            !matches!(event, Event::RealtimeFrame { .. }),
            "a realtime poll interleaved with the write conversation"
        );
    }
    panic!("the write never finished");
}

#[test]
fn a_link_loss_during_a_write_is_reported_and_halts() {
    let dir = TempDir::new("link-loss");
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect(SimConfig {
        faults: vec![oxtuner_core::FaultRule::once(
            SimOp::WritePage,
            Fault::Disconnect,
        )],
        ..SimConfig::default()
    });
    core.read_all();
    core.edit_cell(88.0);

    let event = core.write();
    let (code, _) = failure(&event);
    assert_eq!(code, FailureCode::WriteFailed);

    // The session keeps its reconnect loop; a later write is still halted.
    let (code, message) = failure(&core.write());
    assert_eq!(code, FailureCode::WriteRefused);
    assert!(message.contains("halted"), "{message}");

    core.close();
}

#[test]
fn snapshots_are_listed_through_the_core() {
    let dir = TempDir::new("snapshots");
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect(SimConfig::default());
    core.read_all();
    core.edit_cell(44.0);
    let event = core.write();
    assert!(matches!(event, Event::TuneWritten { .. }), "{event:?}");

    let op = core.handle.list_snapshots().expect("queued");
    let Event::SnapshotsListed { paths, .. } = core.wait(|event| event.op() == Some(op)) else {
        panic!("expected a snapshot list");
    };
    assert!(!paths.is_empty());
    assert!(paths.iter().all(|path| path.exists()));

    core.close();
}

#[test]
fn burn_without_a_connection_is_refused() {
    let dir = TempDir::new("not-connected");
    let core = Harness::start(true);
    core.open(dir.path());

    let (code, _) = failure(&core.burn());
    assert_eq!(code, FailureCode::BurnRefused);

    core.close();
}

#[test]
fn a_write_without_a_project_is_refused() {
    let core = Harness::start(true);
    let op: OpId = core.handle.write_to_ram(Vec::new()).expect("queued");
    let (code, _) = failure(&core.wait(|event| event.op() == Some(op)));
    assert_eq!(code, FailureCode::WriteRefused);

    core.close();
}
