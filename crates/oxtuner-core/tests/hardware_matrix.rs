//! The staged whole-tune matrix on the bench ECU.
//!
//! The stages run in order and each one is its own ignored test, because the
//! power cycle and the pulled cable are manual:
//!
//! ```text
//! stage_format         S0-S6  baseline, read, save the ECU tune into the
//!                             project, import the old tune, write the whole
//!                             tune to RAM, CRC it, burn it
//! stage_cold_boot      S7     power-cycle, then prove flash by CRC
//! stage_restore        S8a    revert to the saved tune, write, burn
//! stage_restore_check  S8b    power-cycle, then prove the baseline is back
//! stage_interrupt      S9a    edit a cell and burn, then pull the cable
//! stage_after_interrupt S9b   replug, check, recover, burn, power-cycle
//! stage_final_check    S9c    prove the baseline is back
//! ```
//!
//! ```text
//! OX_TUNER_WRITE=1 OX_TUNER_BURN=1 \
//! OX_TUNER_PROJECT="/path/to/project copy source" \
//! cargo test -p oxtuner-core --test hardware_matrix -- --ignored --nocapture --test-threads=1
//! ```
//!
//! The first stage copies the project into `OX_TUNER_MATRIX_DIR` (a temp folder
//! by default) and works there, so the source project is never opened for
//! writing. Every stage keeps its CRC table in `<matrix>/evidence.json`.

#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "an ignored hardware run reports each stage and a failed call is a failed stage"
)]

use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use oxtuner_core::{
    BlockingEventStream, BurnReport, CoreConfig, CoreHandle, CoreReport, CoreThread, Event,
    FailureCode, JournalEntry, LatestFrame, LinkMode, OpId, PortChoice, ReadReport, SaveReport,
    TableView, TuneStatus, WriteReport, start,
};
use oxtuner_ini::{Definition, SettingsContext};
use oxtuner_protocol::{BehaviorKind, MsProtocol};
use oxtuner_transport::{SerialSettings, SerialTransport, Transport};
use serde_json::{Map, Value};

const WAIT: Duration = Duration::from_secs(120);
const TABLE: &str = "veTable1Tbl";
const SECOND_TABLE: &str = "afrTable1Tbl";

struct Harness {
    handle: CoreHandle,
    thread: CoreThread,
    events: BlockingEventStream,
    latest: LatestFrame,
}

impl Harness {
    fn start() -> Self {
        let parts = start(&CoreConfig {
            writes_enabled: true,
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

    /// Waits for any event that answers `op`, a failure included.
    fn wait_raw(&self, op: OpId, window: Duration) -> Option<Event> {
        let deadline = Instant::now() + window;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }

            let event = self.events.recv_timeout(left)?;
            if event.op() == Some(op) {
                return Some(event);
            }
        }
    }

    fn open(&self, path: PathBuf) -> oxtuner_core::ProjectView {
        let op = self.handle.open_project(path).expect("queued");
        match self.wait("project open", |event| event.op() == Some(op)) {
            Event::ProjectOpened { view, .. } => view,
            other => panic!("expected a project open, got {other:?}"),
        }
    }

    fn connect(&self, port: &str) -> oxtuner_core::ConnectionView {
        let op = self
            .handle
            .connect(PortChoice::serial(port, 115_200), framing())
            .expect("queued");
        match self.wait("connect", |event| event.op() == Some(op)) {
            Event::Connected { view, .. } => {
                println!("session: {} framing", view.framing);
                view
            }
            other => panic!("expected a connection, got {other:?}"),
        }
    }

    fn read_all(&self) -> ReadReport {
        let op = self.handle.read_all().expect("queued");
        match self.wait("page read", |event| event.op() == Some(op)) {
            Event::PagesRead { report, .. } => report,
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

    fn save(&self) -> SaveReport {
        let op = self.handle.save_tune().expect("queued");
        match self.wait("save", |event| event.op() == Some(op)) {
            Event::TuneSaved { outcome, .. } => outcome,
            other => panic!("expected a save, got {other:?}"),
        }
    }

    fn import(&self, path: &Path) -> oxtuner_core::ImportReport {
        let op = self.handle.import_tune(path).expect("queued");
        match self.wait("import", |event| event.op() == Some(op)) {
            Event::TuneImported { report, .. } => report,
            Event::Failed { failure, .. } => panic!(
                "the import was refused: {} ({:?})",
                failure.message, failure.code
            ),
            other => panic!("expected an import, got {other:?}"),
        }
    }

    fn status(&self) -> TuneStatus {
        let op = self.handle.refresh_tune_status().expect("queued");
        match self.wait("tune status", |event| event.op() == Some(op)) {
            Event::TuneStatusRead { status, .. } => status,
            other => panic!("expected a tune status, got {other:?}"),
        }
    }

    /// Starts realtime polling, which the burn wait needs to see `burnPending`.
    ///
    /// A session that already polls answers the same state, so a refusal is
    /// not an error here.
    fn ensure_realtime(&self) {
        let Ok(op) = self.handle.start_realtime(50) else {
            return;
        };

        match self.wait_raw(op, Duration::from_secs(10)) {
            Some(Event::RealtimeState { running: true, .. }) => {
                println!("S9: realtime polling runs, so the burn waits per page");
            }
            Some(Event::Failed { failure, .. }) => {
                println!("S9: realtime was already running ({})", failure.message);
            }
            other => panic!("expected a realtime state, got {other:?}"),
        }
    }

    fn table(&self, name: &str) -> TableView {
        let op = self.handle.read_table(name).expect("queued");
        match self.wait("table read", |event| event.op() == Some(op)) {
            Event::TableRead { table, .. } => table,
            other => panic!("expected a table read, got {other:?}"),
        }
    }

    fn edit(&self, variable: &str, row: usize, column: usize, value: f64) {
        let op = self
            .handle
            .edit_table(
                variable,
                vec![oxtuner_core::CellEdit { row, column, value }],
            )
            .expect("queued");
        match self.wait("table edit", |event| event.op() == Some(op)) {
            Event::TableEdited { .. } => {}
            other => panic!("expected a table edit, got {other:?}"),
        }
    }

    fn write(&self) -> WriteReport {
        let op = self.handle.write_to_ram(Vec::new()).expect("queued");
        match self.wait("write", |event| event.op() == Some(op)) {
            Event::TuneWritten { report, .. } => report,
            other => panic!("expected a write, got {other:?}"),
        }
    }

    fn burn(&self) -> BurnReport {
        let op = self.handle.burn_to_flash(Vec::new()).expect("queued");
        match self.wait("burn", |event| event.op() == Some(op)) {
            Event::TuneBurned { report, .. } => report,
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

    /// Waits for a realtime frame and returns its rpm.
    fn rpm(&self) -> f64 {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(frame) = self.latest.get()
                && let Some(rpm) = frame.value("rpm")
            {
                return rpm.value;
            }
            assert!(Instant::now() < deadline, "no realtime frame arrived");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn stop(self) -> CoreReport {
        self.thread.join()
    }
}

fn env_required(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("set {name} for a hardware run"))
}

/// The different whole tune the matrix writes and burns.
///
/// It must differ from the ECU's tune in many cells, so the write covers most
/// pages; a project copy of the same tune would only exercise a handful.
fn other_tune() -> PathBuf {
    PathBuf::from(env_required("OX_TUNER_OTHER_TUNE"))
}

fn port() -> String {
    env::var("OX_TUNER_SERIAL_PORT").unwrap_or_else(|_| "/dev/cu.usbmodem2101".to_owned())
}

/// A framing override, for a diagnostic run against the legacy query.
///
/// The matrix itself runs the definition's own framing (`msEnvelope_1.0` for
/// both reference definitions), because the legacy burn command is unproven on
/// newer firmware and a silently refused burn would poison the flash proof.
fn framing() -> Option<LinkMode> {
    match env::var("OX_TUNER_LINK").as_deref() {
        Ok("legacy") => Some(LinkMode::Legacy),
        _ => None,
    }
}

fn matrix_dir() -> PathBuf {
    env::var_os("OX_TUNER_MATRIX_DIR").map_or_else(
        || env::temp_dir().join("oxtuner-matrix-speeduino"),
        PathBuf::from,
    )
}

/// Whether the run is allowed to write at all.
fn writing_allowed() -> bool {
    if env::var("OX_TUNER_WRITE").as_deref() != Ok("1") {
        eprintln!("skipped: set OX_TUNER_WRITE=1 to allow ECU writes");
        return false;
    }

    true
}

/// Whether the run is allowed to burn.
fn burning_allowed() -> bool {
    if env::var("OX_TUNER_BURN").as_deref() != Ok("1") {
        eprintln!("skipped: set OX_TUNER_BURN=1 to allow a flash burn");
        return false;
    }

    true
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;

    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }

    Ok(())
}

/// Copies the bench project into the matrix folder once, keeping the original.
fn prepare_project() -> (PathBuf, PathBuf) {
    let dir = matrix_dir();
    let project = dir.join("project");
    let before = dir.join("tune-before.msq");

    if !project.join("CurrentTune.msq").exists() {
        let source = PathBuf::from(env_required("OX_TUNER_PROJECT"));
        copy_dir(&source, &project).expect("the project copies");
        fs::copy(project.join("CurrentTune.msq"), &before).expect("the before tune copies");
    }

    (project, before)
}

fn definition_of(project: &Path) -> Definition {
    let path = project.join("projectCfg/definition.ini");
    let bytes = fs::read(&path).unwrap_or_else(|error| {
        panic!("could not read {}: {error}", path.display());
    });

    oxtuner_ini::parse_bytes(&bytes, &SettingsContext::new())
        .expect("the project definition parses")
}

/// Every page CRC, read over a fresh protocol connection.
///
/// Only called while no core session is up: the core is the sole owner of the
/// port while it is connected.
fn page_crcs(port: &str, definition: &Definition) -> Vec<(u8, u32)> {
    let behavior = BehaviorKind::select(definition).expect("a known family");
    let mut protocol = MsProtocol::new(definition, behavior.as_dyn()).expect("the protocol builds");
    let mut transport = SerialTransport::new(SerialSettings::new(port).with_baud(115_200));
    transport.open().expect("the port opens");

    // The Arduino resets on open, so wait for the firmware before asking.
    std::thread::sleep(Duration::from_millis(1_500));

    let mut crcs = Vec::new();
    for page in &definition.pages {
        let crc = protocol
            .read_page_crc(&mut transport, page.number)
            .unwrap_or_else(|error| panic!("page {} CRC failed: {error}", page.number))
            .unwrap_or_else(|| panic!("page {} declares no CRC command", page.number));
        crcs.push((page.number, crc));
    }

    transport.close();
    crcs
}

/// The CRCs the run has recorded so far, per stage.
fn evidence() -> Map<String, Value> {
    fs::read(matrix_dir().join("evidence.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

fn record(stage: &str, crcs: &[(u8, u32)]) {
    let mut all = evidence();
    let pages: BTreeMap<String, String> = crcs
        .iter()
        .map(|(page, crc)| (page.to_string(), format!("{crc:08x}")))
        .collect();
    all.insert(
        stage.to_owned(),
        serde_json::to_value(pages).expect("pages serialize"),
    );
    fs::create_dir_all(matrix_dir()).expect("the evidence folder exists");
    fs::write(
        matrix_dir().join("evidence.json"),
        serde_json::to_vec_pretty(&Value::Object(all)).expect("evidence serializes"),
    )
    .expect("the evidence file is written");
}

fn recorded(stage: &str) -> Vec<(u8, u32)> {
    let all = evidence();
    let pages = all
        .get(stage)
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("no {stage} CRCs recorded; run the earlier stage first"));

    let mut crcs: Vec<(u8, u32)> = pages
        .iter()
        .map(|(page, crc)| {
            let crc = crc.as_str().unwrap_or_else(|| panic!("bad CRC for {page}"));
            (
                page.parse().expect("a page number"),
                u32::from_str_radix(crc, 16).expect("a hex CRC"),
            )
        })
        .collect();

    // The evidence file is a JSON object, so its keys come back as text; page
    // order is what the live read produces.
    crcs.sort_by_key(|(page, _)| *page);
    crcs
}

fn print_crcs(label: &str, crcs: &[(u8, u32)]) {
    let line: Vec<String> = crcs
        .iter()
        .map(|(page, crc)| format!("p{page}=0x{crc:08x}"))
        .collect();
    println!("{label}: {}", line.join(" "));
}

/// Two cells that must follow the tune: the first and last cell of a table.
/// A value one step away from the original, inside the cell's declared range.
fn shifted(value: f64) -> f64 {
    if value >= 250.0 {
        value - 2.0
    } else {
        value + 1.0
    }
}

fn endpoints(table: &TableView) -> (f64, f64) {
    let first = table.cells[0];
    let last = table.cells[table.rows * table.cols - 1];

    (first, last)
}

#[test]
#[ignore = "writes the whole tune to the bench ECU and burns it; read the module docs"]
#[expect(
    clippy::too_many_lines,
    reason = "one ordered stage is clearer as one sequence"
)]
fn stage_format() {
    if !writing_allowed() || !burning_allowed() {
        return;
    }

    // A rerun starts from a clean copy: the previous run left the folder's tune
    // already saved, and the save stage must have something to rewrite. A
    // folder this run did not create is never deleted.
    let dir = matrix_dir();
    if dir.join("evidence.json").exists() {
        let _ = fs::remove_dir_all(&dir);
    } else {
        assert!(
            !dir.exists() || fs::read_dir(&dir).is_ok_and(|mut entries| entries.next().is_none()),
            "{} is not empty and was not made by this test; set OX_TUNER_MATRIX_DIR to a fresh folder",
            dir.display()
        );
        let _ = fs::remove_dir_all(&dir);
    }

    let (project, _) = prepare_project();
    let definition = definition_of(&project);
    let other = other_tune();
    println!("matrix: {}", matrix_dir().display());
    println!("other tune: {}", other.display());

    // S0 - baseline, with a byte check for every page.
    let baseline = page_crcs(&port(), &definition);
    assert_eq!(baseline.len(), definition.pages.len());
    print_crcs("S0", &baseline);
    record("s0", &baseline);

    // S1 - session, whole read, engine off.
    let harness = Harness::start();
    let opened = harness.open(project.clone());
    assert!(!opened.read_only, "the project opened read-only");
    let connection = harness.connect(&port());
    assert_eq!(connection.signature, "speeduino 202501");
    assert!(!connection.read_only, "the session is read-only");
    assert!(connection.writes_enabled, "writes must start enabled");
    if framing().is_none() {
        assert_eq!(
            connection.framing, "envelope",
            "the definition declares msEnvelope_1.0; the matrix runs it"
        );
    }

    let read = harness.read_all();
    assert_eq!(read.pages, 15);
    assert_eq!(read.skipped, 0, "every constant must land");
    let rpm = harness.rpm();
    assert!(rpm < 1000.0, "the engine is running: rpm {rpm}");

    harness.adopt();
    let adopted = harness.status();
    println!(
        "S1: {} pages, {} bytes, {} constants, rpm {rpm}, {} cells pending",
        read.pages, read.bytes, read.constants, adopted.pending_write.cells
    );

    // S2 - the ECU tune becomes the project tune.
    let before_hash = file_hash(&project.join("CurrentTune.msq"));
    let saved = harness.save();
    let after_hash = file_hash(&project.join("CurrentTune.msq"));
    assert_ne!(before_hash, after_hash, "the save did not rewrite the tune");

    assert!(saved.cells > 0, "the save wrote nothing");
    let settled = harness.status();
    assert_eq!(settled.unsaved.cells, 0, "the save left unsaved cells");
    assert_eq!(
        settled.pending_write.cells, 0,
        "the save left cells pending"
    );
    println!(
        "S2: saved {} cell(s) to {}, hash {} -> {}",
        saved.cells,
        saved.path.display(),
        before_hash,
        after_hash
    );

    // S3 - the old whole tune goes back into the working image.
    let imported = harness.import(&other);
    let table_after = endpoints(&harness.table(TABLE));
    let second_after = endpoints(&harness.table(SECOND_TABLE));
    let pending = harness.status().pending_write;
    assert!(
        pending.cells > 0,
        "the other tune must differ from the ECU at the wire"
    );
    println!(
        "S3: imported {} cell(s) in {} variable(s), {} pending",
        imported.cells, imported.variables, pending.cells
    );

    // S4 - the whole tune to RAM.
    let written = harness.write();
    assert!(written.verified, "the read-back did not match");
    assert!(written.pages > 0, "the write reported no page");
    assert_eq!(
        written.cells, pending.cells,
        "the write must carry exactly the wire delta the import left pending"
    );

    let table_written = endpoints(&harness.table(TABLE));
    let second_written = endpoints(&harness.table(SECOND_TABLE));
    assert_eq!(table_written, table_after, "the ECU table is not tune A");
    assert_eq!(
        second_written, second_after,
        "the second table is not tune A"
    );
    println!(
        "S4: {} page(s), {} cell(s), verified; {} -> {:?}, {} -> {:?}",
        written.pages, written.cells, TABLE, table_written, SECOND_TABLE, second_written
    );

    let after_write = harness.status();
    assert_eq!(
        after_write.pending_write.cells, 0,
        "the write left cells pending: {:?}",
        after_write.pending_write.changed_variables
    );
    println!("S4b: 0 pending after the write");

    // S5 - the whole tune to flash, in this same session. A port reopen resets
    // the board and the firmware reloads its config from EEPROM, so a RAM change
    // cannot be seen across a disconnect; the burn is what makes it durable.
    let burned = harness.burn();
    assert!(burned.verified, "the post-burn read-back differed");
    assert!(burned.pages > 1, "a whole-tune burn must cover pages");
    let entries = harness.journal();
    assert!(
        entries.iter().any(|entry| entry.operation == "burn"
            && entry.phase == "result"
            && entry.result == "ok"),
        "the journal has no successful burn result"
    );
    harness.disconnect();
    let report = harness.stop();
    assert_eq!(report.verification_failures, 0);

    println!(
        "S5: burned {} page(s), verified, {} writes, {} burns, {} verifications",
        burned.pages, report.writes, report.burns, report.verifications
    );

    // S6 - the flash the board boots from holds tune A. A reconnect is enough
    // here because the reset reloads EEPROM; `stage_cold_boot` proves the same
    // bytes survive a cold start.
    let burned_crcs = page_crcs(&port(), &definition);
    print_crcs("S6", &burned_crcs);
    let moved: Vec<u8> = baseline
        .iter()
        .zip(&burned_crcs)
        .filter(|(before, after)| before.1 != after.1)
        .map(|(before, _)| before.0)
        .collect();
    assert_eq!(
        moved.len(),
        written.pages,
        "the CRCs moved on {:?}, the report wrote {} page(s)",
        moved,
        written.pages
    );
    record("burned", &burned_crcs);

    println!("next: power-cycle the board, then run stage_cold_boot");
}

fn file_hash(path: &Path) -> String {
    use std::hash::{Hash as _, Hasher as _};

    let bytes = fs::read(path).expect("the tune file is readable");
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);

    format!("{:016x}", hasher.finish())
}

#[test]
#[ignore = "needs the power-cycled bench ECU; proves flash by page CRC"]
fn stage_cold_boot() {
    let (project, _) = prepare_project();
    let definition = definition_of(&project);

    let cold = page_crcs(&port(), &definition);
    print_crcs("cold", &cold);
    assert_eq!(
        cold,
        recorded("burned"),
        "the cold boot does not hold the tune that was burned"
    );
    println!("S7: flash holds the written tune; next run stage_restore");
}

#[test]
#[ignore = "writes and burns the whole saved tune back; read the module docs"]
fn stage_restore() {
    if !writing_allowed() || !burning_allowed() {
        return;
    }

    let (project, _) = prepare_project();
    let harness = Harness::start();
    harness.open(project);
    harness.connect(&port());
    // The write needs the ECU image; opening the project already put the
    // project's tune in the working image, which is what gets written.
    harness.read_all();
    assert!(harness.rpm() < 1000.0, "the engine is running");

    let written = harness.write();
    assert!(written.verified, "the restore write did not verify");
    let burned = harness.burn();
    assert!(burned.verified, "the restore burn did not verify");
    harness.disconnect();
    let report = harness.stop();
    assert_eq!(
        report.verification_failures, 0,
        "a read-back differed during the restore"
    );

    println!(
        "S8: wrote {} page(s) of the project tune, burned {} page(s)",
        written.pages, burned.pages
    );
    println!("next: power-cycle the board, then run stage_restore_check");
}

#[test]
#[ignore = "needs the power-cycled bench ECU; proves the project tune by a full read"]
fn stage_restore_check() {
    let (project, _) = prepare_project();
    let definition = definition_of(&project);

    let restored = page_crcs(&port(), &definition);
    print_crcs("restored", &restored);
    let drifted = differing_pages(&recorded("s0"), &restored);
    println!(
        "S8b: {} page(s) differ from the ECU's own bytes: {drifted:?}",
        drifted.len()
    );

    // The project file cannot carry every byte: `noMsqSave` values are not
    // written to a tune file by any tool. The honest proof is that the ECU now
    // holds exactly what the project says, cell for cell.
    assert_matches_project(&project, &port());
    println!("S8b: the ECU matches the project tune; next run stage_interrupt");
}

/// Asserts a fresh read of the ECU matches the open project at the wire level.
fn assert_matches_project(project: &Path, port: &str) {
    let harness = Harness::start();
    harness.open(project.to_path_buf());
    harness.connect(port);
    harness.read_all();

    let status = harness.status();
    assert_eq!(
        status.pending_write.cells, 0,
        "the ECU does not hold the project tune: {} cell(s) differ",
        status.pending_write.cells
    );

    harness.disconnect();
    let report = harness.stop();
    assert_eq!(
        report.verification_failures, 0,
        "a read-back differed during the check"
    );
}

/// Page numbers whose CRC differs between two snapshots.
fn differing_pages(before: &[(u8, u32)], after: &[(u8, u32)]) -> Vec<u8> {
    before
        .iter()
        .zip(after)
        .filter(|(before, after)| before.1 != after.1)
        .map(|(before, _)| before.0)
        .collect()
}

#[test]
#[ignore = "burns the whole tune and expects the cable to be pulled; read the module docs"]
fn stage_interrupt() {
    if !writing_allowed() || !burning_allowed() {
        return;
    }

    let (project, _) = prepare_project();
    let harness = Harness::start();
    harness.open(project);
    harness.connect(&port());
    harness.read_all();
    assert!(harness.rpm() < 1000.0, "the engine is running");
    harness.adopt();

    // Whatever tune the board holds, one shifted cell guarantees a write and a
    // burn to interrupt. The cell is the high-load corner, like the quick gate.
    let table = harness.table(TABLE);
    let row = table.rows - 1;
    let column = table.cols - 1;
    let original = table.cells[row * table.cols + column];
    harness.edit(&table.name, row, column, shifted(original));

    let written = harness.write();
    assert!(written.verified);

    // Polling makes the burn wait for `burnPending` one page at a time, which
    // is both the real firmware path and the window the cable is pulled in.
    harness.ensure_realtime();
    println!(
        "S9: wrote {} cell(s) over {} page(s); starting the burn",
        written.cells, written.pages
    );

    // The operator starts the burn when the cable is in their hand.
    let go = matrix_dir().join("go");
    let _ = fs::remove_file(&go);
    println!("S9: READY; waiting for {} before the burn", go.display());
    let deadline = Instant::now() + Duration::from_secs(180);
    while !go.exists() {
        assert!(Instant::now() < deadline, "no go file arrived");
        std::thread::sleep(Duration::from_millis(200));
    }

    // A short delay after the go cue gives the operator time to pull while the
    // burn is in flight. A pull just before it still counts: the burn command
    // then fails with the link.
    println!("S9: GO; the burn starts in 3 seconds");
    std::thread::sleep(Duration::from_secs(3));

    let op = harness
        .handle
        .burn_to_flash(Vec::new())
        .expect("the burn is queued");
    println!("S9: BURNING NOW");

    let outcome = harness.wait_raw(op, Duration::from_secs(60));
    match outcome {
        Some(Event::TuneBurned { .. }) => {
            panic!("the burn finished before the cable was pulled; the run must be redone")
        }
        Some(Event::Failed { failure, .. }) => {
            assert!(
                matches!(
                    failure.code,
                    FailureCode::BurnFailed | FailureCode::ConnectFailed
                ),
                "unexpected failure code {:?}: {}",
                failure.code,
                failure.message
            );
            println!("S9: reported {} as expected", failure.code.as_str());
        }
        Some(other) => panic!("unexpected event while the cable was pulled: {other:?}"),
        None => panic!("the burn never answered; a link loss must be reported, not swallowed"),
    }

    println!("next: replug the cable, then run stage_after_interrupt");
}

#[test]
#[ignore = "recovers a tune after a pulled cable; read the module docs"]
fn stage_after_interrupt() {
    if !writing_allowed() || !burning_allowed() {
        return;
    }

    let (project, _) = prepare_project();
    let definition = definition_of(&project);

    let after = page_crcs(&port(), &definition);
    print_crcs("interrupt", &after);
    record("interrupt", &after);

    let harness = Harness::start();
    harness.open(project);
    harness.connect(&port());
    let read = harness.read_all();
    assert!(harness.rpm() < 1000.0, "the engine is running");

    // The project's tune is the target; the read makes the ECU image known so
    // the delta is what the interrupted session left behind.
    let written = harness.write();
    assert!(written.verified, "the recovery write did not verify");
    let burned = harness.burn();
    assert!(burned.verified, "the recovery burn did not verify");
    harness.disconnect();
    let report = harness.stop();
    assert_eq!(
        report.verification_failures, 0,
        "a read-back differed during the recovery"
    );

    println!(
        "S9b: read {} page(s), wrote {} page(s), burned {} page(s)",
        read.pages, written.pages, burned.pages
    );
    println!("next: power-cycle the board, then run stage_final_check");
}

#[test]
#[ignore = "needs the power-cycled bench ECU; proves the baseline by page CRC"]
fn stage_final_check() {
    let (project, _) = prepare_project();
    let definition = definition_of(&project);

    let final_crcs = page_crcs(&port(), &definition);
    print_crcs("final", &final_crcs);
    let drifted = differing_pages(&recorded("s0"), &final_crcs);
    println!(
        "S9c: {} page(s) differ from the ECU's own bytes: {drifted:?}",
        drifted.len()
    );

    assert_matches_project(&project, &port());
    println!("matrix complete: the ECU holds the project tune and every stage passed");
}
