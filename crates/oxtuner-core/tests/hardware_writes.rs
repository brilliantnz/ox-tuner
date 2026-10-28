//! The M7 bench gate: a verified write and a verified burn on the real NO2C.
//!
//! The test is ignored by default and needs two switches, because a burn
//! changes the ECU's flash. Without `OX_TUNER_WRITE=1` it skips. Without
//! `OX_TUNER_BURN=1` it stops after the RAM write.
//!
//! ```text
//! OX_TUNER_WRITE=1 OX_TUNER_BURN=1 \
//! OX_TUNER_PROJECT=/path/to/bench/project \
//! cargo test -p oxtuner-core --test hardware_writes -- --ignored --nocapture
//! ```
//!
//! The project must hold the definition that matches the firmware on the
//! board, and its tune should be close to the ECU's own. The port defaults to
//! `/dev/cu.usbmodem21201` and can be set with `OX_TUNER_SERIAL_PORT`.
//!
//! The cell under test is written back to its original value and burned again,
//! so the ECU keeps the tune it had before the test. The project's `.ox-tuner`
//! sidecar keeps the journal and snapshots as evidence.
//!
//! Run bench tests with `--test-threads=1`: two sessions on one tty interleave
//! their bytes and both time out.

use std::{
    env,
    path::PathBuf,
    time::{Duration, Instant},
};

use oxtuner_core::{
    BlockingEventStream, BurnSummary, CellEdit, ConnectionSummary, CoreConfig, CoreHandle,
    CoreReport, CoreThread, Event, JournalEntry, LinkMode, OpId, PortChoice, ProjectSummary,
    ReadAllSummary, TableSummary, TuneStatus, WriteSummary, start,
};

const WAIT: Duration = Duration::from_secs(120);

/// The table the test edits: a 16x16 VE table the engine only reaches at high
/// load and high RPM, so a one-unit change cannot show up at idle.
const TABLE: &str = "veTable1Tbl";

struct Harness {
    handle: CoreHandle,
    thread: CoreThread,
    events: BlockingEventStream,
}

impl Harness {
    fn start() -> Self {
        let parts = start(CoreConfig {
            writes_enabled: true,
            ..CoreConfig::default()
        })
        .expect("the core starts");
        Self {
            handle: parts.handle,
            thread: parts.thread,
            events: parts.events.into_blocking(),
        }
    }

    fn wait(&self, what: &str, wanted: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(event) = self.events.recv_timeout(left) else {
                panic!("no {what} answer before the deadline");
            };
            if let Event::Failed { failure, .. } = &event
                && !wanted(&event)
            {
                panic!("{what} failed: {} ({:?})", failure.message, failure.code);
            }
            if wanted(&event) {
                return event;
            }
        }
    }

    fn open(&self, path: PathBuf) -> ProjectSummary {
        let op = self.handle.open_project(path).expect("queued");
        match self.wait("project open", |event| event.op() == Some(op)) {
            Event::ProjectOpened { summary, .. } => summary,
            other => panic!("expected a project open, got {other:?}"),
        }
    }

    fn connect(&self, port: &str, framing: Option<LinkMode>) -> ConnectionSummary {
        let op = self
            .handle
            .connect(PortChoice::serial(port, 115_200), framing)
            .expect("queued");
        match self.wait("connect", |event| event.op() == Some(op)) {
            Event::Connected { summary, .. } => summary,
            other => panic!("expected a connection, got {other:?}"),
        }
    }

    fn read_all(&self) -> ReadAllSummary {
        let op = self.handle.read_all().expect("queued");
        match self.wait("page read", |event| event.op() == Some(op)) {
            Event::PagesRead { summary, .. } => summary,
            other => panic!("expected a page read, got {other:?}"),
        }
    }

    fn adopt(&self) {
        let op = self.handle.adopt_ecu_image().expect("queued");
        match self.wait("adopt", |event| event.op() == Some(op)) {
            Event::EcuAdopted { .. } => {}
            other => panic!("expected an adopt, got {other:?}"),
        }
    }

    fn status(&self) -> TuneStatus {
        let op = self.handle.refresh_tune_status().expect("queued");
        match self.wait("tune status", |event| event.op() == Some(op)) {
            Event::TuneStatusRead { status, .. } => status,
            other => panic!("expected a tune status, got {other:?}"),
        }
    }

    fn table(&self) -> TableSummary {
        let op = self.handle.read_table(TABLE).expect("queued");
        match self.wait("table read", |event| event.op() == Some(op)) {
            Event::TableRead { table, .. } => table,
            other => panic!("expected a table read, got {other:?}"),
        }
    }

    fn edit(&self, variable: &str, row: usize, column: usize, value: f64) {
        let op = self
            .handle
            .edit_table(variable, vec![CellEdit { row, column, value }])
            .expect("queued");
        match self.wait("table edit", |event| event.op() == Some(op)) {
            Event::TableEdited { .. } => {}
            other => panic!("expected a table edit, got {other:?}"),
        }
    }

    fn write(&self) -> WriteSummary {
        let op = self.handle.write_to_ram(Vec::new()).expect("queued");
        match self.wait("write", |event| event.op() == Some(op)) {
            Event::TuneWritten { summary, .. } => summary,
            other => panic!("expected a write, got {other:?}"),
        }
    }

    fn burn(&self) -> BurnSummary {
        let op = self.handle.burn_to_flash(Vec::new()).expect("queued");
        match self.wait("burn", |event| event.op() == Some(op)) {
            Event::TuneBurned { summary, .. } => summary,
            other => panic!("expected a burn, got {other:?}"),
        }
    }

    fn journal(&self) -> Vec<JournalEntry> {
        let op: OpId = self.handle.read_journal().expect("queued");
        match self.wait("journal read", |event| event.op() == Some(op)) {
            Event::JournalRead { entries, .. } => entries,
            other => panic!("expected a journal, got {other:?}"),
        }
    }

    fn disconnect(&self) {
        let op = self.handle.disconnect().expect("queued");
        match self.wait("disconnect", |event| event.op() == Some(op)) {
            Event::Disconnected { .. } => {}
            other => panic!("expected a disconnect, got {other:?}"),
        }
    }

    fn stop(self) -> CoreReport {
        self.thread.join()
    }
}

fn project_path() -> PathBuf {
    match env::var("OX_TUNER_PROJECT") {
        Ok(path) => PathBuf::from(path),
        Err(_) => panic!("set OX_TUNER_PROJECT to the bench project's folder"),
    }
}

fn port() -> String {
    env::var("OX_TUNER_SERIAL_PORT").unwrap_or_else(|_| "/dev/cu.usbmodem21201".to_owned())
}

fn framing() -> Option<LinkMode> {
    match env::var("OX_TUNER_LINK").as_deref() {
        Ok("legacy") => Some(LinkMode::Legacy),
        _ => None,
    }
}

/// A value one step away from the original, inside the cell's declared range.
fn shifted(value: f64) -> f64 {
    if value >= 250.0 {
        value - 2.0
    } else {
        value + 1.0
    }
}

#[test]
#[ignore = "writes to the bench ECU's RAM and flash; see the module docs"]
fn the_bench_ecu_verifies_a_write_and_a_burn_and_leaves_the_tune_as_found() {
    if env::var("OX_TUNER_WRITE").as_deref() != Ok("1") {
        eprintln!("skipped: set OX_TUNER_WRITE=1 to allow a RAM write");
        return;
    }
    let burn_allowed = env::var("OX_TUNER_BURN").as_deref() == Ok("1");

    let harness = Harness::start();

    let project = harness.open(project_path());
    assert!(
        !project.read_only,
        "the bench project opened read-only: {:?}",
        project.read_only_reasons
    );
    println!(
        "project: {} · {} values loaded · {} skipped",
        project.definition.signature, project.loaded, project.skipped
    );

    let connection = harness.connect(&port(), framing());
    assert_eq!(connection.signature, "speeduino 202501");
    assert!(!connection.read_only, "the session is read-only (S1)");
    assert!(connection.writes_enabled, "writes must start enabled here");
    println!(
        "connected: {} · {} · {} pages · realtime {:?}",
        connection.port, connection.framing, connection.pages, connection.realtime_block
    );

    let read = harness.read_all();
    assert_eq!(read.skipped, 0, "every constant must land");
    assert_eq!(read.pages, 15);
    println!(
        "pages: {} · bytes {} · constants {}",
        read.pages, read.bytes, read.constants
    );

    // The project file and the ECU hold different tunes. Adopting the ECU image
    // makes the read the working tune, so the table below is what the
    // controller holds and the write is the edit and nothing else.
    harness.adopt();
    let adopted = harness.status();
    assert!(
        adopted.pending_write.cells <= 4,
        "adopting left {} unsent cells; the write would replace the ECU tune",
        adopted.pending_write.cells
    );
    println!(
        "adopted: {} unsent cell(s), {} pending burn",
        adopted.pending_write.cells, adopted.pending_burn.cells
    );

    let table = harness.table();
    let row = table.rows - 1;
    let column = table.cols - 1;
    let index = row * table.cols + column;
    let original = table.cells[index];
    let changed = shifted(original);
    println!(
        "cell: {}[{}][{}] {} -> {} {}",
        TABLE, row, column, original, changed, table.units
    );

    harness.edit(&table.name, row, column, changed);
    let edit = harness.status();
    let after_edit = harness.table();
    println!(
        "after edit: cell {} · unsaved {} · pending write {} · state {:?}",
        after_edit.cells[index],
        edit.unsaved.cells,
        edit.pending_write.cells,
        after_edit.states[index]
    );
    assert_eq!(
        edit.pending_write.cells,
        adopted.pending_write.cells + 1,
        "the edit must add exactly one unsent cell"
    );

    let written = harness.write();
    assert!(
        written.verified,
        "the read-back did not match the write (S3)"
    );
    assert_eq!(
        written.cells,
        adopted.pending_write.cells + 1,
        "the delta patch must send only the adopted differences and the edit"
    );
    println!(
        "write: {} page(s), {} cell(s), verified, snapshot {:?}, journal {:?}",
        written.pages, written.cells, written.snapshot, written.journal
    );

    let after_write = harness.table();
    assert_eq!(
        after_write.cells[index], changed,
        "ECU RAM kept the old value"
    );

    if !burn_allowed {
        // Put RAM back without touching flash, so the next session starts clean.
        harness.edit(&table.name, row, column, original);
        let restored = harness.write();
        assert!(restored.verified);
        println!("burn: skipped (set OX_TUNER_BURN=1); RAM restored to {original}");
        harness.disconnect();
        let report = harness.stop();
        assert_eq!(report.verification_failures, 0);
        return;
    }

    let burned = harness.burn();
    assert!(burned.verified, "the post-burn read-back differed");
    println!(
        "burn: {} page(s), verified, snapshot {:?}",
        burned.pages, burned.snapshot
    );

    // Leave the ECU as it was found, through the same verified path.
    harness.edit(&table.name, row, column, original);
    let restored = harness.write();
    assert!(restored.verified, "the restore write did not verify");
    let restored_burn = harness.burn();
    assert!(restored_burn.verified, "the restore burn did not verify");
    let after_restore = harness.table();
    assert_eq!(
        after_restore.cells[index], original,
        "the tune was not restored"
    );
    println!(
        "restored: {}[{}][{}] back to {} and burned",
        TABLE, row, column, original
    );

    let entries = harness.journal();
    for entry in entries
        .iter()
        .filter(|entry| entry.operation == "write" || entry.operation == "burn")
    {
        println!(
            "journal: {} {} {} · {} · {} cells · {}",
            entry.operation, entry.phase, entry.result, entry.pages, entry.cells, entry.detail
        );
    }
    assert!(
        entries.iter().any(|entry| entry.operation == "burn"
            && entry.phase == "result"
            && entry.result == "ok"),
        "the journal has no successful burn result"
    );

    harness.disconnect();
    let report = harness.stop();
    assert_eq!(report.verification_failures, 0, "a read-back differed");
    println!(
        "report: {} writes · {} burns · {} verifications · {} verification failures",
        report.writes, report.burns, report.verifications, report.verification_failures
    );
}
