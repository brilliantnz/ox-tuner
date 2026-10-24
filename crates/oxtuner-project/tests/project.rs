//! End-to-end tests for opening, editing and saving a TunerStudio project.
//!
//! The fixture is a real project shape in miniature: `projectCfg/` with an
//! `#include`, a `CurrentTune.msq` with unknown elements, and a definition with
//! a scalar, a bit field, text and a table. Every test runs in its own temporary
//! directory, so the filesystem assertions ("only these files were written")
//! are meaningful.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use oxtuner_project::hash::{hex, sha256};
use oxtuner_project::{IssueKind, Project, ProjectError, TUNE_FILE};

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

const PAGE_TWO_WITH_EXTRA: &str = "page = 2
b = scalar, U08, 0, \"\", 1, 0, 0, 255, 0
extra = scalar, U08, 1, \"\", 1, 0, 0, 255, 0
";

const PAGE_ONE_MSQ: &str = r#"    <page number="1" size="288">
        <constant name="b">2.0</constant>
    </page>
"#;

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

/// A temporary directory that removes itself, even when a test fails.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "oxtuner-project-{label}-{}-{sequence}",
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
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn project_dir(label: &str) -> TempDir {
    let dir = TempDir::new(label);
    dir.write("projectCfg/mainController.ini", MAIN_DEFINITION);
    dir.write("projectCfg/page2.ini", PAGE_TWO);
    dir.write(TUNE_FILE, TUNE);
    dir
}

fn read_tune(dir: &TempDir) -> Vec<u8> {
    fs::read(dir.path().join(TUNE_FILE)).expect("the tune file is readable")
}

fn tune_text(dir: &TempDir) -> String {
    String::from_utf8(read_tune(dir)).expect("the fixture is UTF-8")
}

fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "{actual} is not close to {expected}"
    );
}

#[test]
fn open_edit_save_reopen_round_trips_every_value_shape() {
    let dir = project_dir("round-trip");
    let mut project = Project::open(dir.path()).expect("the project opens");

    assert!(project.is_in_project());
    assert!(!project.is_read_only(), "{:?}", project.report().blockers);
    assert!(
        project.report().warnings.is_empty(),
        "{:?}",
        project.report().warnings
    );
    assert_eq!(project.definition().pages.len(), 2);
    assert_eq!(project.report().loaded, 6);
    assert_close(project.tune().scalar_user("reqFuel").unwrap(), 15.4);
    assert_eq!(project.tune().text("note").unwrap(), "NO2C");

    project.tune_mut().set_user("reqFuel", 12.3).unwrap();
    project.tune_mut().set_user("aeMode", 2.0).unwrap();
    project.tune_mut().set_text("note", "NEW").unwrap();
    project
        .tune_mut()
        .set_cell_user("table", 0, 1, 20.0)
        .unwrap();

    let outcome = project.save().expect("the edit is saveable");
    let mut variables = outcome.variables.clone();
    variables.sort();
    assert_eq!(variables, ["aeMode", "note", "reqFuel", "table"]);
    assert_eq!(outcome.cells, 4);
    assert_eq!(outcome.path, dir.path().join(TUNE_FILE));

    let saved = tune_text(&dir);
    assert!(saved.contains("<!-- a comment this build does not understand and must keep -->"));
    assert!(saved.contains("<futureThing mode=\"x\"><nested a=\"1\"/></futureThing>"));
    assert!(saved.contains("<bibliography author=\"TunerStudio\" tuneComment=\"fixture\"/>"));
    assert!(saved.contains("<constant name=\"aeMode\">\"Alpha-N\"</constant>"));
    assert!(saved.contains("<constant name=\"note\">\"NEW\"</constant>"));

    let reopened = Project::open(dir.path()).expect("the saved project reopens");
    assert!(!reopened.is_read_only());
    assert_close(reopened.tune().scalar_user("reqFuel").unwrap(), 12.3);
    assert_eq!(reopened.tune().scalar_raw("aeMode").unwrap(), 2.0);
    assert_eq!(reopened.tune().text("note").unwrap(), "NEW");
    assert_close(reopened.tune().cell_user("table", 0, 1).unwrap(), 20.0);
}

#[test]
fn saving_without_edits_leaves_the_tune_byte_identical() {
    let dir = project_dir("no-op");
    let before = read_tune(&dir);
    let mut project = Project::open(dir.path()).expect("the project opens");

    let outcome = project.save().expect("an unedited project saves");
    assert_eq!(outcome.cells, 0);
    assert!(outcome.variables.is_empty());
    assert_eq!(read_tune(&dir), before);

    // A second save after committing the baseline is still a no-op.
    let outcome = project.save().expect("the project saves again");
    assert_eq!(outcome.cells, 0);
    assert_eq!(read_tune(&dir), before);
}

#[test]
fn a_save_records_provenance_inside_the_sidecar() {
    let dir = project_dir("provenance");
    let definition_bytes =
        fs::read(dir.path().join("projectCfg/mainController.ini")).expect("the INI is readable");
    let mut project = Project::open(dir.path()).expect("the project opens");
    assert!(project.provenance().is_none());

    project.save().expect("the project saves");

    let provenance = project.provenance().expect("provenance was written");
    assert_eq!(provenance.definition, "projectCfg/mainController.ini");
    assert_eq!(
        provenance.definition_sha256,
        hex(&sha256(&definition_bytes))
    );
    assert_eq!(provenance.signature, "speeduino 202501");
}

#[test]
fn a_write_stays_inside_the_project_and_its_sidecar() {
    let sandbox = TempDir::new("fs-scope");
    let root = sandbox.path().join("project");
    fs::create_dir_all(root.join("projectCfg")).expect("project directory is creatable");
    fs::write(root.join("projectCfg/mainController.ini"), MAIN_DEFINITION).unwrap();
    fs::write(root.join("projectCfg/page2.ini"), PAGE_TWO).unwrap();
    fs::write(root.join(TUNE_FILE), TUNE).unwrap();

    let mut project = Project::open(&root).expect("the project opens");
    project.tune_mut().set_user("reqFuel", 9.5).unwrap();
    project.save().expect("the edit saves");

    let mut written = Vec::new();
    collect_paths(sandbox.path(), sandbox.path(), &mut written);
    written.sort();
    assert_eq!(
        written,
        [
            "project",
            "project/.ox-tuner",
            "project/.ox-tuner/provenance",
            "project/CurrentTune.msq",
            "project/projectCfg",
            "project/projectCfg/mainController.ini",
            "project/projectCfg/page2.ini",
        ]
    );
    assert!(
        !written.iter().any(|path| path.contains("oxtuner-tmp")),
        "the temporary file must be renamed away"
    );
}

fn collect_paths(root: &Path, current: &Path, into: &mut Vec<String>) {
    for entry in fs::read_dir(current).expect("directory is readable") {
        let path = entry.expect("directory entry is readable").path();
        into.push(
            path.strip_prefix(root)
                .expect("path is inside the root")
                .to_string_lossy()
                .replace('\\', "/"),
        );
        if path.is_dir() {
            collect_paths(root, &path, into);
        }
    }
}

type ReadOnlyCase = (&'static str, fn(&str) -> String, IssueKind);

#[test]
fn structural_mismatches_open_read_only_and_refuse_to_save() {
    let cases: Vec<ReadOnlyCase> = vec![
        (
            "signature",
            |tune| tune.replace("speeduino 202501", "speeduino 202412"),
            IssueKind::SignatureMismatch,
        ),
        (
            "page-size",
            |tune| tune.replace("size=\"288\"", "size=\"256\""),
            IssueKind::PageSizeMismatch,
        ),
        (
            "missing-page",
            |tune| tune.replace(PAGE_ONE_MSQ, ""),
            IssueKind::MissingPage,
        ),
        (
            "page-count",
            |tune| tune.replace("nPages=\"2\"", "nPages=\"3\""),
            IssueKind::PageCountMismatch,
        ),
        (
            "shape",
            |tune| tune.replace("cols=\"2\"", "cols=\"4\""),
            IssueKind::ShapeMismatch,
        ),
    ];

    for (label, transform, kind) in cases {
        let dir = TempDir::new(label);
        dir.write("projectCfg/mainController.ini", MAIN_DEFINITION);
        dir.write("projectCfg/page2.ini", PAGE_TWO);
        dir.write(TUNE_FILE, &transform(TUNE));
        let before = read_tune(&dir);

        let mut project = Project::open(dir.path()).expect("the tune opens read-only");
        assert!(project.is_read_only(), "{label} must gate saving");
        assert!(
            project
                .report()
                .blockers
                .iter()
                .any(|issue| issue.kind == kind),
            "{label}: {:?}",
            project.report().blockers
        );

        match project.save() {
            Err(ProjectError::ReadOnly { reasons }) => {
                assert!(!reasons.is_empty(), "{label} explains why");
            }
            other => panic!("{label} must refuse to save, got {other:?}"),
        }
        assert_eq!(read_tune(&dir), before, "{label} must not touch the file");
    }
}

#[test]
fn an_entry_missing_from_the_tune_is_appended_on_save() {
    let dir = TempDir::new("missing-entry");
    dir.write("projectCfg/mainController.ini", MAIN_DEFINITION);
    dir.write("projectCfg/page2.ini", PAGE_TWO_WITH_EXTRA);
    dir.write(TUNE_FILE, TUNE);

    let mut project = Project::open(dir.path()).expect("the project opens");
    assert!(!project.is_read_only());
    assert!(
        project
            .report()
            .warnings
            .iter()
            .any(|issue| issue.kind == IssueKind::MissingValue && issue.message.contains("extra")),
        "{:?}",
        project.report().warnings
    );

    project
        .tune_mut()
        .set_user("extra", 7.0)
        .expect("extra is declared");

    let outcome = project.save().expect("the new value is saveable");
    assert_eq!(outcome.variables, ["extra"]);
    assert_eq!(outcome.cells, 1);
    assert!(tune_text(&dir).contains("<constant name=\"extra\">7.0</constant>"));

    let reopened = Project::open(dir.path()).expect("the saved project reopens");
    assert!(!reopened.is_read_only());
    assert_close(reopened.tune().scalar_user("extra").unwrap(), 7.0);
    assert!(
        !reopened
            .report()
            .warnings
            .iter()
            .any(|issue| issue.kind == IssueKind::MissingValue)
    );
}

#[test]
fn a_standalone_tune_uses_its_settings_snapshot_and_has_no_sidecar() {
    const STANDALONE_DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
#if CELSIUS
tempUnits = scalar, U08, 0, "", 1, 0, 0, 255, 0
#endif
"#;
    const STANDALONE_TUNE: &str = r#"<msq>
    <versionInfo fileFormat="5.0" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="64">
        <constant name="tempUnits">1.0</constant>
    </page>
    <settings Comment="written by TunerStudio">
        <setting name="CELSIUS" value="CELSIUS"/>
    </settings>
</msq>
"#;

    let dir = TempDir::new("standalone");
    dir.write("standalone.ini", STANDALONE_DEFINITION);
    dir.write("tune.msq", STANDALONE_TUNE);

    let tune_path = dir.path().join("tune.msq");
    let definition_path = dir.path().join("standalone.ini");
    let mut project =
        Project::open_standalone(&tune_path, &definition_path).expect("the tune opens");
    assert!(!project.is_in_project());
    assert!(project.settings().is_active("CELSIUS"));
    assert!(!project.is_read_only(), "{:?}", project.report().blockers);
    assert_close(project.tune().scalar_user("tempUnits").unwrap(), 1.0);
    assert!(project.provenance().is_none());

    project.tune_mut().set_user("tempUnits", 4.0).unwrap();
    let outcome = project.save().expect("the edit saves");
    assert_eq!(outcome.variables, ["tempUnits"]);
    assert!(project.provenance().is_none());
    assert!(!dir.path().join(".ox-tuner").exists());
}

#[test]
fn a_project_also_reads_the_settings_snapshot() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
#if CELSIUS
tempUnits = scalar, U08, 0, "", 1, 0, 0, 255, 0
#endif
"#;
    const TUNE: &str = r#"<msq>
    <versionInfo fileFormat="5.0" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="64">
        <constant name="tempUnits">1.0</constant>
    </page>
    <settings>
        <setting name="CELSIUS" value="CELSIUS"/>
    </settings>
</msq>
"#;

    let dir = TempDir::new("project-settings");
    dir.write("projectCfg/mainController.ini", DEFINITION);
    dir.write(TUNE_FILE, TUNE);

    let mut project = Project::open(dir.path()).expect("the project opens");
    assert!(project.settings().is_active("CELSIUS"));
    assert!(!project.is_read_only(), "{:?}", project.report().blockers);
    assert_close(project.tune().scalar_user("tempUnits").unwrap(), 1.0);

    let before = read_tune(&dir);
    let outcome = project.save().expect("an unedited project saves");
    assert_eq!(outcome.cells, 0);
    assert_eq!(read_tune(&dir), before);

    // Without the snapshot the flag is off, so the definition omits the
    // constant and the stale entry is a warning, not a blocker.
    dir.write(
        TUNE_FILE,
        &TUNE.replace(
            "        <setting name=\"CELSIUS\" value=\"CELSIUS\"/>\n",
            "",
        ),
    );
    let project = Project::open(dir.path()).expect("the project still opens");
    assert!(!project.settings().is_active("CELSIUS"));
    assert!(!project.is_read_only());
    assert!(
        project
            .report()
            .warnings
            .iter()
            .any(|issue| issue.kind == IssueKind::OrphanValue)
    );
}

#[test]
fn a_malformed_tune_is_an_error_not_a_panic() {
    let dir = project_dir("malformed");
    dir.write(
        TUNE_FILE,
        "<msq><versionInfo signature=\"speeduino 202501\">",
    );
    assert!(matches!(
        Project::open(dir.path()),
        Err(ProjectError::Msq(_))
    ));

    dir.write(TUNE_FILE, "<tune/>");
    assert!(matches!(
        Project::open(dir.path()),
        Err(ProjectError::Msq(_))
    ));
}

/// The reference-project check runs only on the owner's machine.
///
/// Set `OXTUNER_REFERENCE_PROJECT` to a TunerStudio project directory and run
/// `cargo test -p oxtuner-project -- --ignored`. The project is copied first,
/// so the original is never written to.
#[test]
#[ignore = "needs a local TunerStudio project (OXTUNER_REFERENCE_PROJECT)"]
fn a_reference_project_opens_and_round_trips() {
    let Ok(source) = std::env::var("OXTUNER_REFERENCE_PROJECT") else {
        return;
    };
    let source = PathBuf::from(source);
    let dir = TempDir::new("reference");
    let copied = dir.path().join("project");
    copy_tree(&source, &copied);

    let original_tune = fs::read(source.join(TUNE_FILE)).expect("the reference tune is readable");
    let mut project = Project::open(&copied).expect("the reference project opens");

    assert!(!project.is_read_only(), "{:?}", project.report().blockers);
    assert_eq!(project.definition().pages.len(), 15);
    // The tune carries one more `<constant>` than the definition: an orphan
    // that must stay a warning, not a read-only gate.
    assert_eq!(project.definition().constants.len(), 817);
    // The INI declares 50 PC variables; this tune stores the 17 the user set.
    assert_eq!(project.definition().pc_variables.len(), 50);
    assert!(
        project
            .report()
            .warnings
            .iter()
            .any(|issue| issue.kind == IssueKind::OrphanValue)
    );

    let outcome = project.save().expect("an unedited reference project saves");
    assert_eq!(outcome.cells, 0);
    assert_eq!(
        fs::read(copied.join(TUNE_FILE)).expect("the copy is readable"),
        original_tune
    );

    let reopened = Project::open(&copied).expect("the saved copy reopens");
    assert!(!reopened.is_read_only());
    assert_eq!(
        reopened.layout().variables().len(),
        project.layout().variables().len()
    );
}

#[test]
fn a_no_msq_save_variable_is_not_reported_as_written() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 32
page = 1
reqFuel = scalar, U08, 0, "ms", 0.1, 0, 0, 25.5, 1
helper = scalar, U08, 1, "", 1, 0, 0, 255, 0, noMsqSave
"#;
    const TUNE: &str = r#"<msq>
    <versionInfo fileFormat="5.0" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="32">
        <constant name="reqFuel">15.4</constant>
        <constant name="helper">0.0</constant>
    </page>
</msq>
"#;

    let dir = TempDir::new("no-msq-save");
    dir.write("projectCfg/mainController.ini", DEFINITION);
    dir.write(TUNE_FILE, TUNE);

    let mut project = Project::open(dir.path()).expect("the project opens");
    project.tune_mut().set_user("reqFuel", 12.3).unwrap();
    project.tune_mut().set_user("helper", 7.0).unwrap();

    let outcome = project.save().expect("the edit saves");

    assert_eq!(outcome.variables, ["reqFuel"]);
    assert_eq!(outcome.cells, 1);
    let saved = tune_text(&dir);
    assert!(
        saved.contains("<constant name=\"helper\">0.0</constant>"),
        "a noMsqSave variable keeps its file text: {saved}"
    );
}

#[test]
fn a_self_closing_entry_and_settings_block_survive_an_edit() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
reqFuel = scalar, U08, 0, "ms", 0.1, 0, 0, 25.5, 1
"#;
    const TUNE: &str = r#"<msq>
    <versionInfo fileFormat="5.0" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="64"><constant name="reqFuel"/></page>
    <settings/>
</msq>
"#;

    let dir = TempDir::new("self-closing-entry");
    dir.write("projectCfg/mainController.ini", DEFINITION);
    dir.write(TUNE_FILE, TUNE);

    let mut project = Project::open(dir.path()).expect("the project opens");
    assert!(!project.is_read_only(), "{:?}", project.report().blockers);

    project.tune_mut().set_user("reqFuel", 12.3).unwrap();
    let outcome = project.save().expect("the edit saves");

    assert_eq!(outcome.cells, 1);
    let saved = tune_text(&dir);
    assert!(
        saved.contains("<constant name=\"reqFuel\">12.3</constant>"),
        "the value must be written, not dropped by the self-closing tag: {saved}"
    );
    assert!(
        saved.contains("<settings/>"),
        "a settings block with nothing to change keeps its form: {saved}"
    );

    let reopened = Project::open(dir.path()).expect("the saved project reopens");
    assert_close(reopened.tune().scalar_user("reqFuel").unwrap(), 12.3);
}

#[test]
fn a_missing_entry_is_written_into_a_self_closing_page() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
reqFuel = scalar, U08, 0, "ms", 0.1, 0, 0, 25.5, 1
"#;
    const TUNE: &str = r#"<msq>
    <versionInfo fileFormat="5.0" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="64"/>
</msq>
"#;

    let dir = TempDir::new("self-closing-page");
    dir.write("projectCfg/mainController.ini", DEFINITION);
    dir.write(TUNE_FILE, TUNE);

    let mut project = Project::open(dir.path()).expect("the project opens");
    assert!(!project.is_read_only(), "{:?}", project.report().blockers);
    project.tune_mut().set_user("reqFuel", 12.3).unwrap();

    let outcome = project.save().expect("the edit saves");
    assert_eq!(outcome.cells, 1);

    let saved = tune_text(&dir);
    assert!(
        saved.contains("<constant name=\"reqFuel\">12.3</constant>"),
        "the page must be opened before the entry is appended: {saved}"
    );
    let reopened = Project::open(dir.path()).expect("the saved project reopens");
    assert_close(reopened.tune().scalar_user("reqFuel").unwrap(), 12.3);
}

#[test]
fn a_setting_group_choice_does_not_rewrite_an_unedited_tune() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[SettingGroups]
settingGroup = pressure_units, "Pressure Display"
settingOption = DEFAULT, "PSI"
settingOption = pressure_bar, "BAR"

[Constants]
nPages = 1
pageSize = 64
page = 1
a = scalar, U08, 0, "", 1, 0, 0, 255, 0
#if pressure_bar
b = scalar, U08, 1, "", 1, 0, 0, 255, 0
#endif
"#;
    const TUNE: &str = r#"<msq>
    <versionInfo fileFormat="5.0" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="64">
        <constant name="a">1.0</constant>
        <constant name="b">2.0</constant>
    </page>
    <settings>
        <setting name="pressure_units" value="pressure_bar"/>
    </settings>
</msq>
"#;

    let dir = TempDir::new("group-choice");
    dir.write("projectCfg/mainController.ini", DEFINITION);
    dir.write(TUNE_FILE, TUNE);
    let before = read_tune(&dir);

    let mut project = Project::open(dir.path()).expect("the project opens");
    assert!(
        project.settings().is_active("pressure_bar"),
        "the option is active through the entry's value"
    );
    assert!(!project.is_read_only(), "{:?}", project.report().blockers);

    let outcome = project.save().expect("an unedited project saves");

    assert_eq!(outcome.cells, 0);
    assert_eq!(
        read_tune(&dir),
        before,
        "the group option must not be appended as a second setting"
    );
}

#[test]
fn a_failed_save_does_not_change_what_the_session_saves_next() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
a = scalar, U08, 0, "", 1, 0, 0, 255, 0
mode = bits, U08, 1, [0:3], "ONE", "TWO"
"#;
    const TUNE: &str = r#"<msq>
    <versionInfo fileFormat="5.0" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="64">
        <constant name="a">1.0</constant>
        <constant name="mode">"ONE"</constant>
    </page>
</msq>
"#;

    let dir = TempDir::new("failed-save");
    dir.write("projectCfg/mainController.ini", DEFINITION);
    dir.write(TUNE_FILE, TUNE);
    let before = read_tune(&dir);

    let mut project = Project::open(dir.path()).expect("the project opens");
    project.tune_mut().set_user("a", 5.0).unwrap();
    // `a` is formatted first; a bit value with no option label fails after it.
    project.tune_mut().set_raw_at("mode", 0, 99.0).unwrap();

    assert!(matches!(
        project.save(),
        Err(ProjectError::Unrepresentable { .. })
    ));
    assert_eq!(read_tune(&dir), before, "the failed save wrote nothing");

    // Discard the edits: the next save must write the file as it was, not the
    // values the failed attempt had already put into its working copy.
    project.images_mut().revert_to_baseline();
    let outcome = project.save().expect("the reverted tune saves");

    assert_eq!(outcome.cells, 0);
    assert_eq!(read_tune(&dir), before);
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("destination is creatable");
    for entry in fs::read_dir(from).expect("source is readable") {
        let entry = entry.expect("entry is readable");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type is readable").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("file is copyable");
        }
    }
}
