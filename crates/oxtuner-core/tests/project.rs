//! Project lifecycle in the core: open in place, save, read-only, and failure.
//!
//! The fixture is a real project shape in miniature (the same one
//! `oxtuner-project`'s end-to-end tests use): `projectCfg/` with an `#include`,
//! a `CurrentTune.msq` with unknown elements, and a definition with a scalar, a
//! bit field, text and a table. Every test runs in its own temporary directory,
//! so "the file was not touched" assertions are meaningful.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use oxtuner_core::{
    BlockingEventStream, CoreConfig, CoreHandle, CoreThread, Event, FailureCode, OpId, ProjectView,
    SaveReport, start,
};
use oxtuner_project::{CreateRequest, TUNE_FILE};

/// How long a test waits for an event before failing.
const WAIT: Duration = Duration::from_secs(10);

const MAIN_DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 2
pageSize = 128, 288
page = 1
reqFuel = scalar, U08, 0, "ms", 0.1, 0, 0, 25.5, 1
aeMode = bits, U08, 1, [0:7], "TPS", "MAP", "Alpha-N"
note = string, ASCII, 8
table = array, U08, 4, [2x2], "", 1, 0, 0, 255, 0
#include page2.ini

[PcVariables]
rpmhigh = scalar, U16, 0, "rpm", 1, 0, 0, 20000, 0
"#;

const PAGE_TWO: &str = "page = 2
b = scalar, U08, 0, \"\", 1, 0, 0, 255, 0
";

const TUNE: &str = r#"<?xml version="1.0" encoding="ISO-8859-1"?>
<msq xmlns="http://www.msefi.com/:msq">
    <bibliography author="TunerStudio" tuneComment="fixture"/>
    <versionInfo fileFormat="5.0" firmwareInfo="Speeduino+2025.01.3" nPages="2" signature="speeduino 202501"/>
    <page>
        <pcVariable digits="0" name="rpmhigh" units="rpm">12000.0</pcVariable>
    </page>
    <page number="0" size="128">
        <constant digits="1" name="reqFuel" units="ms">15.4</constant>
        <constant name="aeMode">"MAP"</constant>
        <constant name="note">"NO2C"</constant>
        <constant cols="2" digits="0" name="table" rows="2" units="">1.0 2.0 3.0 4.0</constant>
        <!-- a comment this build does not understand and must keep -->
        <futureThing mode="x"><nested a="1"/></futureThing>
    </page>
    <page number="1" size="288">
        <constant name="b">2.0</constant>
    </page>
    <settings Comment="written by TunerStudio">
        <setting name="enablehardware_test_OFF" value="enablehardware_test_OFF"/>
        <setting name="CELSIUS" value="CELSIUS"/>
    </settings>
</msq>
"#;

/// A single-page definition without `#include`, the only shape creation copies.
const CREATE_DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 128
page = 1
reqFuel = scalar, U08, 0, "ms", 0.1, 0, 0, 25.5, 1
"#;

/// Writes the create fixture's definition beside the sandbox and returns it.
fn definition_path(directory: &Path) -> PathBuf {
    let path = directory.join("create-definition.ini");
    fs::write(&path, CREATE_DEFINITION).expect("the definition is writable");
    path
}

/// A definition with one setting group, for the wizard's inspect command.
const INSPECT_DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[SettingGroups]
settingGroup = pressure_units, "Pressure Display"
settingOption = DEFAULT, "PSI"
settingOption = pressure_bar, "BAR"

[Constants]
nPages = 1
pageSize = 128
page = 1

#if CELSIUS
celsius_only = scalar, U08, 8, "", 1, 0, 0, 255, 0
#endif
"#;

/// The same tune, written for another firmware.
fn other_firmware_tune() -> String {
    TUNE.replace("signature=\"speeduino 202501\"", "signature=\"other 1\"")
}

/// A temporary directory that removes itself, even when a test fails.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "oxtuner-core-project-{label}-{}-{sequence}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temporary directory is creatable");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.path.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("fixture parent is creatable");
        }
        fs::write(path, contents).expect("fixture is writable");
    }

    fn tune(&self) -> Vec<u8> {
        fs::read(self.path.join(TUNE_FILE)).expect("the tune file is readable")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn project_dir(label: &str, tune: &str) -> TempDir {
    let dir = TempDir::new(label);
    dir.write("projectCfg/mainController.ini", MAIN_DEFINITION);
    dir.write("projectCfg/page2.ini", PAGE_TWO);
    dir.write(TUNE_FILE, tune);
    dir
}

/// A running core whose events are consumed synchronously.
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

    fn open(&self, path: &Path) -> OpId {
        self.handle
            .open_project(path)
            .expect("the command is queued")
    }

    fn next(&self) -> Event {
        self.events.recv_timeout(WAIT).expect("an event arrives")
    }
}

fn opened(event: &Event) -> &ProjectView {
    match event {
        Event::ProjectOpened { view: summary, .. } => summary,
        other => panic!("expected a project open, got {other:?}"),
    }
}

fn failure(event: &Event) -> (FailureCode, &str) {
    match event {
        Event::Failed { failure, .. } => (failure.code, failure.message.as_str()),
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[test]
fn opening_a_project_summarises_the_tune_for_the_ui() {
    let dir = project_dir("open", TUNE);
    let core = Harness::start();

    let op = core.open(dir.path());
    let event = core.next();

    assert_eq!(event.op(), Some(op), "the reply carries the command's id");
    let summary = opened(&event);
    assert_eq!(summary.root.as_path(), dir.path());
    assert_eq!(summary.tune_path, dir.path().join(TUNE_FILE));
    assert!(summary.in_project);
    assert!(!summary.read_only, "{:?}", summary.read_only_reasons);
    assert_eq!(
        summary.read_only_reasons,
        Vec::<String>::new(),
        "a writable project has no read-only reasons"
    );
    assert!(
        summary.read_only_reason.is_none(),
        "a writable project allocates no joined reason"
    );
    assert_eq!(summary.definition.signature, "speeduino 202501");
    assert_eq!(summary.definition.pages, 2);
    assert_eq!(summary.definition.constants, 5);
    // Six entries exist in the tune: five constants and one PC variable.
    assert_eq!(summary.loaded, 6);
    assert_eq!(summary.skipped, 0);
    assert!(summary.warnings.is_empty(), "{:?}", summary.warnings);
}

#[test]
fn saving_an_unedited_project_writes_nothing_and_keeps_the_file_identical() {
    let dir = project_dir("save", TUNE);
    let before = dir.tune();
    let core = Harness::start();

    core.open(dir.path());
    opened(&core.next());

    let op = core.handle.save_tune().expect("the command is queued");
    let event = core.next();

    assert_eq!(event.op(), Some(op));
    let Event::TuneSaved { outcome, .. } = &event else {
        panic!("expected a save, got {event:?}");
    };
    let expected = SaveReport {
        path: dir.path().join(TUNE_FILE),
        cells: 0,
    };
    assert_eq!(outcome, &expected);
    assert_eq!(dir.tune(), before, "an unedited save is byte-identical");
}

#[test]
fn a_signature_mismatch_opens_read_only_and_refuses_to_save() {
    let dir = project_dir("read-only", &other_firmware_tune());
    let before = dir.tune();
    let core = Harness::start();

    core.open(dir.path());
    let summary = opened(&core.next()).clone();

    assert!(summary.read_only);
    assert!(
        !summary.read_only_reasons.is_empty(),
        "read-only mode must explain itself (S1)"
    );
    assert!(
        summary
            .read_only_reason
            .as_deref()
            .is_some_and(|reason| !reason.is_empty()),
        "the joined reason is precomputed for the editor"
    );

    let op = core.handle.save_tune().expect("the command is queued");
    let event = core.next();

    assert_eq!(event.op(), Some(op));
    let (kind, message) = failure(&event);
    assert_eq!(kind, FailureCode::TuneSaveFailed);
    assert_ne!(message, "", "the refusal names a reason");
    assert_eq!(dir.tune(), before, "a refused save touches nothing");
}

#[test]
fn a_failed_open_keeps_the_previous_project_open() {
    let dir = project_dir("keep", TUNE);
    let core = Harness::start();

    core.open(dir.path());
    opened(&core.next());

    let missing = dir.path().join("does-not-exist");
    let op = core.open(&missing);
    let event = core.next();

    assert_eq!(event.op(), Some(op));
    let (kind, message) = failure(&event);
    assert_eq!(kind, FailureCode::ProjectRejected);
    assert!(
        message.contains("does-not-exist"),
        "the message names the path: {message}"
    );

    // The old project survived the failure, so saving still works.
    core.handle.save_tune().expect("the command is queued");
    let Event::TuneSaved { outcome, .. } = core.next() else {
        panic!("expected the previous project to still be open");
    };
    assert_eq!(outcome.cells, 0);
}

#[test]
fn saving_without_a_project_says_so() {
    let core = Harness::start();

    let op = core.handle.save_tune().expect("the command is queued");
    let event = core.next();

    assert_eq!(event.op(), Some(op));
    let (kind, message) = failure(&event);
    assert_eq!(kind, FailureCode::NothingToSave);
    assert_ne!(message, "", "the refusal names a reason");
}

#[test]
fn unloading_closes_the_project() {
    let dir = project_dir("unload", TUNE);
    let core = Harness::start();

    core.open(dir.path());
    opened(&core.next());

    let op = core
        .handle
        .unload_definition()
        .expect("the command is queued");
    assert_eq!(core.next(), Event::DefinitionUnloaded { op });

    core.handle.save_tune().expect("the command is queued");
    let (kind, _) = failure(&core.next());
    assert_eq!(kind, FailureCode::NothingToSave);
}

#[test]
fn a_project_failure_is_not_a_dead_core() {
    let empty = TempDir::new("empty");
    let core = Harness::start();

    let op = core.open(empty.path());
    assert_eq!(core.next().op(), Some(op));

    // The core still answers: a definition load works after a rejected project.
    let op = core
        .handle
        .load_definition("speeduino.ini", MAIN_DEFINITION.as_bytes())
        .expect("the command is queued");
    let event = core.next();
    assert_eq!(event.op(), Some(op));
    assert!(matches!(event, Event::DefinitionLoaded { .. }));
}

#[test]
fn creating_a_project_scaffolds_it_and_leaves_it_open() {
    let dir = TempDir::new("create");
    let core = Harness::start();

    let op = core
        .handle
        .create_project(CreateRequest {
            parent: dir.path().to_path_buf(),
            name: "Roadster".to_owned(),
            ini_path: definition_path(dir.path()),
            expected_signature: Some("speeduino 202501".to_owned()),
            settings: Vec::new(),
            port: None,
            baud: 0,
        })
        .expect("the command is queued");
    let event = core.next();

    assert_eq!(event.op(), Some(op));
    let summary = opened(&event);
    let root = dir.path().join("Roadster");
    assert_eq!(summary.root, root);
    assert!(summary.in_project);
    assert!(!summary.read_only, "{:?}", summary.read_only_reasons);
    assert_eq!(summary.definition.signature, "speeduino 202501");
    assert!(root.join(TUNE_FILE).is_file());
    assert!(root.join("projectCfg/mainController.ini").is_file());
    assert!(root.join(".oxtuner/project.json").is_file());

    // The created project is a live session: Save answers like any other.
    let op = core.handle.save_tune().expect("the save is queued");
    let event = core.next();
    assert_eq!(event.op(), Some(op));
    assert!(matches!(event, Event::TuneSaved { .. }), "{event:?}");
}

#[test]
fn inspect_definition_answers_with_the_setting_groups() {
    let dir = TempDir::new("inspect");
    let path = dir.path().join("groups.ini");
    fs::write(&path, INSPECT_DEFINITION).expect("the definition is writable");
    let core = Harness::start();

    let op = core
        .handle
        .inspect_definition(&path)
        .expect("the command is queued");
    let event = core.next();

    assert_eq!(event.op(), Some(op));
    match event {
        Event::DefinitionInspected {
            path: answered,
            preview,
            ..
        } => {
            assert_eq!(answered, path);
            assert_eq!(preview.signature, "speeduino 202501");
            assert_eq!(preview.groups.len(), 1);
            assert_eq!(preview.groups[0].label, "Pressure Display");
            assert_eq!(preview.groups[0].default.as_deref(), Some("DEFAULT"));
            assert!(preview.tested_flags.iter().any(|flag| flag == "celsius"));
        }
        other => panic!("expected an inspected definition, got {other:?}"),
    }
}

#[test]
fn a_created_project_remembers_the_wizard_connection() {
    let dir = TempDir::new("create-connection");
    let core = Harness::start();

    let op = core
        .handle
        .create_project(CreateRequest {
            parent: dir.path().to_path_buf(),
            name: "Roadster".to_owned(),
            ini_path: definition_path(dir.path()),
            expected_signature: None,
            settings: Vec::new(),
            port: Some("/dev/cu.test".to_owned()),
            baud: 57_600,
        })
        .expect("the command is queued");
    let event = core.next();

    assert_eq!(event.op(), Some(op));
    let summary = opened(&event);
    assert_eq!(
        summary.connection.last_port.as_deref(),
        Some("/dev/cu.test")
    );
    assert_eq!(summary.connection.baud_override, Some(57_600));
}

#[test]
fn a_refused_create_reports_the_reason_and_writes_nothing() {
    let dir = TempDir::new("create-refused");
    let core = Harness::start();

    let op = core
        .handle
        .create_project(CreateRequest {
            parent: dir.path().to_path_buf(),
            name: "Mismatch".to_owned(),
            ini_path: definition_path(dir.path()),
            expected_signature: Some("other firmware".to_owned()),
            settings: Vec::new(),
            port: None,
            baud: 0,
        })
        .expect("the command is queued");
    let event = core.next();

    assert_eq!(event.op(), Some(op));
    let (reason, message) = failure(&event);
    assert_eq!(reason, FailureCode::ProjectRejected);
    assert!(message.contains("other firmware"), "{message}");
    assert!(
        !dir.path().join("Mismatch").exists(),
        "a refused create leaves no folder behind"
    );
}

#[test]
fn the_project_view_carries_the_remembered_connection() {
    let dir = project_dir("device", TUNE);
    dir.write(
        ".oxtuner/project.json",
        r#"{"schema_version": 1, "connection": {"last_port": "/dev/ttyACM0", "baud_override": 115200}}"#,
    );

    let core = Harness::start();
    let _ = core.open(dir.path());
    let summary = opened(&core.next()).clone();

    assert_eq!(
        summary.connection.last_port.as_deref(),
        Some("/dev/ttyACM0")
    );
    assert_eq!(summary.connection.baud_override, Some(115_200));
}

#[test]
fn a_project_without_state_has_no_remembered_connection() {
    let dir = project_dir("no-device", TUNE);

    let core = Harness::start();
    let _ = core.open(dir.path());
    let summary = opened(&core.next()).clone();

    assert_eq!(summary.connection.last_port, None);
    assert_eq!(summary.connection.baud_override, None);
}

#[test]
fn the_project_layout_is_stored_in_its_own_state_file() {
    let dir = project_dir("layout", TUNE);
    let core = Harness::start();

    let _ = core.open(dir.path());
    let summary = opened(&core.next()).clone();
    assert!(
        summary.ui.primary_sidebar_visible,
        "a fresh project starts open"
    );

    let ui = oxtuner_project::UiState {
        primary_sidebar_width: 300.0,
        secondary_sidebar_width: 260.0,
        bottom_panel_height: 240.0,
        primary_sidebar_visible: false,
        secondary_sidebar_visible: true,
        bottom_panel_visible: true,
        table_low_at_top: true,
    };
    core.handle
        .set_project_ui(ui.clone())
        .expect("the layout command is queued");
    let ack = core.next();
    assert!(
        matches!(ack, Event::ProjectUiSaved { .. }),
        "the layout command answers: {ack:?}"
    );

    // Commands run in order on the core thread, so the save proves the layout
    // write finished before the file is read.
    let op = core.handle.save_tune().expect("the save is queued");
    let event = core.next();
    assert_eq!(event.op(), Some(op));

    let state = oxtuner_project::ProjectState::read(dir.path())
        .expect("the state is readable")
        .expect("the state was written");
    assert_eq!(state.ui, ui);
}

#[test]
fn a_layout_command_with_no_project_open_is_ignored() {
    let dir = project_dir("layout-no-project", TUNE);
    let core = Harness::start();

    core.handle
        .set_project_ui(oxtuner_project::UiState::default())
        .expect("the command is queued");
    let ack = core.next();
    assert!(
        matches!(ack, Event::ProjectUiSaved { .. }),
        "a command with no project still answers: {ack:?}"
    );
    assert!(!dir.path().join(".oxtuner").exists());
}
