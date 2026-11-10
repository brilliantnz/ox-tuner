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
    LatestFrame, OpId, ProjectView, start,
};
use oxtuner_project::TUNE_FILE;
use oxtuner_sim::{Fault, FaultRule, SimConfig, SimOp};

mod support;

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
status = scalar, U08, 3, "bits", 1.0, 0.0
burnPending = bits, U08, 3, [4:4]

[ControllerCommands]
cmdPing = "z"
cmdChain = "z", "w"

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
    latest: LatestFrame,
}

impl Harness {
    fn start(writes_enabled: bool) -> Self {
        let parts = start(&CoreConfig {
            writes_enabled,
            ..CoreConfig::default()
        })
        .expect("the core starts");
        Self {
            handle: parts.handle,
            thread: parts.thread,
            events: parts.events.into_blocking(),
            latest: parts.latest,
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

    fn open(&self, path: &Path) -> ProjectView {
        let op = self.handle.open_project(path).expect("queued");
        match self.wait(|event| event.op() == Some(op)) {
            Event::ProjectOpened { view: summary, .. } => summary,
            other => panic!("expected a project open, got {other:?}"),
        }
    }

    fn connect(&self, config: SimConfig) -> oxtuner_core::ConnectionView {
        self.connect_with(config, None)
    }

    fn connect_with(
        &self,
        config: SimConfig,
        framing: Option<oxtuner_core::LinkMode>,
    ) -> oxtuner_core::ConnectionView {
        let op = self
            .handle
            .connect(support::simulator_with(config), framing)
            .expect("queued");
        match self.wait(|event| event.op() == Some(op)) {
            Event::Connected { view: summary, .. } => summary,
            other => panic!("expected a connection, got {other:?}"),
        }
    }

    fn read_all(&self) -> oxtuner_core::ReadReport {
        let op = self.handle.read_all().expect("queued");
        match self.wait(|event| event.op() == Some(op)) {
            Event::PagesRead { report, .. } => {
                assert_eq!(report.skipped, 0, "every constant must be placed");
                report
            }
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
            Event::TableEdited { table, .. } => {
                assert_eq!(table.cells[0], value, "the edit must reach the table");
            }
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

    fn resolve_conflicts(&self, resolution: oxtuner_core::ConflictResolution) -> Event {
        let op = self
            .handle
            .execute(oxtuner_core::CoreCommand::Tune(
                oxtuner_core::TuneCommand::ResolveConflicts { resolution },
            ))
            .expect("queued");
        self.wait(|event| event.op() == Some(op))
    }

    fn run_command(&self, command: &str) -> Event {
        let op = self
            .handle
            .run_controller_command("engineDialog", command)
            .expect("queued");
        self.wait(|event| event.op() == Some(op))
    }

    fn journal(&self) -> (Option<PathBuf>, Vec<JournalEntry>) {
        let op = self.handle.read_journal().expect("queued");
        match self.wait(|event| event.op() == Some(op)) {
            Event::JournalRead { path, entries, .. } => (path, entries),
            other => panic!("expected a journal, got {other:?}"),
        }
    }

    fn status(&self) -> oxtuner_core::TuneStatus {
        let op = self.handle.refresh_tune_status().expect("queued");
        match self.wait(|event| event.op() == Some(op)) {
            Event::TuneStatusRead { status, .. } => status,
            other => panic!("expected a tune status, got {other:?}"),
        }
    }

    fn table(&self) -> oxtuner_core::TableView {
        let op = self.handle.read_table("veTable").expect("queued");
        match self.wait(|event| event.op() == Some(op)) {
            Event::TableRead { table, .. } => table,
            other => panic!("expected a table, got {other:?}"),
        }
    }

    fn adopt(&self) {
        let op = self.handle.adopt_ecu_image().expect("queued");
        match self.wait(|event| event.op() == Some(op)) {
            Event::EcuAdopted { .. } => {}
            other => panic!("expected an adopt, got {other:?}"),
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

    let Event::TuneWritten {
        report: summary, ..
    } = event
    else {
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
    let (kind, message) = failure(&core.write());
    assert_eq!(kind, FailureCode::WriteRefused);
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
fn an_ecu_value_outside_the_declared_range_is_left_alone() {
    let dir = TempDir::new("range");
    // The controller holds a bin past the definition's declared maximum: 65535
    // rpm against 10000. Adopting its tune must not clamp that value, because
    // the definition's range is an editing limit, not the firmware's. The write
    // plan is the edit and nothing else.
    let mut page = [0u8; 16];
    page[4..6].copy_from_slice(&u16::MAX.to_le_bytes());
    let config = SimConfig {
        pages: vec![(1, page.to_vec())],
        ..SimConfig::default()
    };
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect(config);
    core.read_all();
    core.adopt();
    assert_eq!(core.status().pending_write.cells, 0);

    core.edit_cell(77.0);
    let Event::TuneWritten {
        report: summary, ..
    } = core.write()
    else {
        panic!("expected a verified write");
    };
    assert!(summary.verified);
    assert_eq!(
        summary.cells, 1,
        "the ECU's out-of-range bin must not be clamped into the write"
    );

    core.close();
}

#[test]
fn a_page_that_moved_under_the_writer_is_refused() {
    let dir = TempDir::new("drift");
    // The settle loop reads page 1 twice at connect; the third read is the
    // write's own base, and it comes back with a different last byte. The
    // writer must see that the page moved and stop before sending anything.
    let config = SimConfig {
        faults: vec![
            FaultRule::new(
                SimOp::ReadPage,
                Fault::Delay {
                    delay: Duration::ZERO,
                },
                2,
            ),
            FaultRule::once(SimOp::ReadPage, Fault::Corrupt { at: 15 }),
        ],
        ..SimConfig::default()
    };
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect_with(config, Some(oxtuner_core::LinkMode::Legacy));
    core.read_all();
    core.edit_cell(77.0);

    let (kind, message) = failure(&core.write());
    assert_eq!(kind, FailureCode::WriteRefused);
    assert!(
        message.contains("page(s) 1 changed on the ECU since the last read"),
        "{message}"
    );

    // Nothing reached the controller: a fresh read still holds the old tune, so
    // the edit is still one cell away and the next write verifies.
    core.read_all();
    let Event::TuneWritten {
        report: summary, ..
    } = core.write()
    else {
        panic!("the write after a fresh read must succeed");
    };
    assert!(summary.verified);
    assert!(summary.cells >= 1);
    assert_eq!(core.table().cells[0], 77.0);

    let (_, entries) = core.journal();
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.phase == "result" && entry.result == "ok")
            .count(),
        1,
        "only the second write may record a result"
    );

    core.close();
}

#[test]
fn taking_the_ecu_image_makes_the_next_write_one_cell() {
    let dir = TempDir::new("adopt");
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect(SimConfig::default());
    core.read_all();

    // The simulator's image is the ECU's tune. Adopting it clears the
    // differences the project file had with the controller.
    core.adopt();
    let adopted = core.status();
    assert!(
        adopted.pending_write.cells <= 2,
        "adopting left {} unsent cells",
        adopted.pending_write.cells
    );

    core.edit_cell(77.0);
    let status = core.status();
    assert_eq!(
        status.pending_write.cells,
        adopted.pending_write.cells + 1,
        "one edit must add exactly one unsent cell"
    );

    let Event::TuneWritten {
        report: summary, ..
    } = core.write()
    else {
        panic!("expected a verified write");
    };
    assert!(summary.verified);

    core.close();
}

#[test]
fn adopting_without_a_read_is_refused() {
    let dir = TempDir::new("adopt-refused");
    let core = Harness::start(true);
    core.open(dir.path());

    let op = core.handle.adopt_ecu_image().expect("queued");
    let (kind, message) = failure(&core.wait(|event| event.op() == Some(op)));
    assert_eq!(kind, FailureCode::AdoptRefused);
    assert!(message.contains("read the ECU pages"), "{message}");

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

    let (kind, message) = failure(&core.write());
    assert_eq!(kind, FailureCode::WriteRefused);
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

    let (kind, message) = failure(&core.write());
    assert_eq!(kind, FailureCode::WriteRefused);
    assert!(message.contains("S1"), "{message}");

    core.close();
}

#[test]
fn a_partial_write_fails_verification_and_halts_the_path() {
    let dir = TempDir::new("partial");
    let core = Harness::start(true);
    core.open(dir.path());
    core.connect(SimConfig {
        faults: vec![FaultRule::once(
            SimOp::WritePage,
            Fault::PartialWrite { bytes: 0 },
        )],
        ..SimConfig::default()
    });
    core.read_all();
    core.edit_cell(90.0);

    let event = core.write();
    let (kind, message) = failure(&event);
    assert_eq!(kind, FailureCode::WriteFailed, "{message}");
    assert!(message.contains("read-back"), "{message}");

    let table = core.table();
    assert_eq!(
        table.states[0],
        oxtuner_core::CellState::WriteFailed,
        "the changed cell is marked failed"
    );

    // The halt is a latch: a second write must not reach the wire even though
    // writes are still enabled.
    let (kind, message) = failure(&core.write());
    assert_eq!(kind, FailureCode::WriteRefused);
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

    let (kind, message) = failure(&core.burn());
    assert_eq!(kind, FailureCode::BurnRefused);
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
    let Event::TuneReverted {
        report: summary, ..
    } = event
    else {
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
    let (kind, message) = failure(&core.wait(|event| event.op() == Some(op)));
    assert_eq!(kind, FailureCode::ImportNeedsConsent);
    assert!(message.contains("consent"), "{message}");

    let op = core.handle.import_tune(&other, true).expect("queued");
    let event = core.wait(|event| event.op() == Some(op));
    let Event::TuneImported {
        report: summary, ..
    } = event
    else {
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
    let (kind, _) = failure(&core.wait(|event| event.op() == Some(op)));
    assert_eq!(kind, FailureCode::ImportRejected);

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
        faults: vec![FaultRule::once(
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
    // A frame published before the write is expected; a newer one would mean a
    // poll ran inside the write conversation.
    let baseline = core.latest.get().map_or(0, |frame| frame.sequence);
    // The poll schedule starts while the write transaction is already active.
    // If the scheduler let a poll through, a newer frame would arrive before the
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
        assert_eq!(
            core.latest.get().map_or(0, |frame| frame.sequence),
            baseline,
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
        faults: vec![FaultRule::once(SimOp::WritePage, Fault::Disconnect)],
        ..SimConfig::default()
    });
    core.read_all();
    core.edit_cell(88.0);

    let event = core.write();
    let (kind, _) = failure(&event);
    assert_eq!(kind, FailureCode::WriteFailed);

    // The session keeps its reconnect loop; a later write is still halted.
    let (kind, message) = failure(&core.write());
    assert_eq!(kind, FailureCode::WriteRefused);
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
    assert_ne!(
        paths,
        Vec::<PathBuf>::new(),
        "the project must have a snapshot"
    );
    assert!(paths.iter().all(|path| path.exists()));

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

#[test]
fn a_write_without_a_project_is_refused() {
    let core = Harness::start(true);
    let op: OpId = core.handle.write_to_ram(Vec::new()).expect("queued");
    let (kind, _) = failure(&core.wait(|event| event.op() == Some(op)));
    assert_eq!(kind, FailureCode::WriteRefused);

    core.close();
}

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
