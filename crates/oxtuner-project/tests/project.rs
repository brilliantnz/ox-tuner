//! End-to-end tests for opening, editing and saving a `TunerStudio` project.
//!
//! The fixture is a real project shape in miniature: `projectCfg/` with an
//! `#include`, a `CurrentTune.msq` with unknown elements, and a definition with
//! a scalar, a bit field, text and a table. Every test runs in its own temporary
//! directory, so the filesystem assertions ("only these files were written")
//! are meaningful.

#![expect(
    clippy::unwrap_used,
    reason = "integration tests may unwrap; a failed call is a failed test"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use oxtuner_project::hash::{hex, sha256};
use oxtuner_project::{
    ConnectionSeed, CreateRequest, IssueKind, Project, ProjectError, ProjectLayout, ProjectState,
    TUNE_FILE, WritePolicy,
};
use uuid::Uuid;

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
table = array, U08, 16, [2x2], "", 1, 0, 0, 255, 0
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

/// A single-page definition without includes, for standalone and creation tests.
/// A definition with no includes, for tests that only need a scaffold.
const CONNECTION_DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
reqFuel = scalar, U08, 0, "ms", 0.1, 0, 0, 25.5, 1
"#;

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

/// The tune matching the standalone definition.
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

    /// Writes a definition into the sandbox and returns the path creation takes.
    fn definition(&self, contents: &str) -> PathBuf {
        self.write("definition.ini", contents);
        self.path.join("definition.ini")
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
    assert!(saved.contains("<constant name=\"note\">NEW</constant>"));

    let reopened = Project::open(dir.path()).expect("the saved project reopens");
    assert!(!reopened.is_read_only());
    assert_close(reopened.tune().scalar_user("reqFuel").unwrap(), 12.3);
    assert_eq!(reopened.tune().scalar_raw("aeMode").unwrap(), 2.0);
    assert_eq!(reopened.tune().text("note").unwrap(), "NEW");
    assert_close(reopened.tune().cell_user("table", 0, 1).unwrap(), 20.0);
}

#[test]
fn a_snapshot_renders_the_working_image_without_touching_the_tune() {
    let dir = project_dir("snapshot");
    let mut project = Project::open(dir.path()).expect("the project opens");
    let before = read_tune(&dir);
    project.tune_mut().set_user("reqFuel", 9.9).expect("known");
    project
        .tune_mut()
        .set_cell_user("table", 1, 1, 42.0)
        .expect("known");

    let bytes = project.snapshot_msq().expect("the snapshot renders");
    assert_eq!(read_tune(&dir), before, "the project file is untouched");

    let snapshot = dir.path().join("snapshot.msq");
    fs::write(&snapshot, &bytes).expect("snapshot written");
    let reopened =
        Project::open_standalone(&snapshot, dir.path().join("projectCfg/mainController.ini"))
            .expect("the snapshot opens as a tune");
    assert_close(reopened.tune().scalar_user("reqFuel").unwrap(), 9.9);
    assert_close(reopened.tune().cell_user("table", 1, 1).unwrap(), 42.0);
}

#[test]
fn a_snapshot_keeps_the_file_text_of_a_value_it_cannot_render() {
    let dir = TempDir::new("unresolved-scale");
    dir.write(
        "projectCfg/mainController.ini",
        r#"[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 128
page = 1
reqFuel = scalar, U08, 0, "ms", 0.1, 0, 0, 25.5, 1
iacMin = scalar, U08, 1, "%", {idleRes}, 0.0, 0.0, 100, 0
"#,
    );
    dir.write(
        TUNE_FILE,
        r#"<?xml version="1.0" encoding="ISO-8859-1"?>
<msq xmlns="http://www.msefi.com/:msq">
    <versionInfo fileFormat="5.0" firmwareInfo="Speeduino+2025.01.3" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="128">
        <constant digits="1" name="reqFuel" units="ms">15.4</constant>
        <constant name="iacMin" units="%">12.0</constant>
    </page>
</msq>
"#,
    );

    let mut project = Project::open(dir.path()).expect("the project opens");
    project
        .tune_mut()
        .set_user("reqFuel", 9.9)
        .expect("a resolved value is editable");

    let bytes = project
        .snapshot_msq()
        .expect("one unresolved expression must not block the snapshot");
    let text = String::from_utf8(bytes).expect("the snapshot is UTF-8");
    assert!(
        text.contains(r#"<constant name="iacMin" units="%">12.0</constant>"#),
        "the file text of the unresolved value was lost: {text}"
    );
    assert!(
        text.contains(">9.9<"),
        "the edited value is missing: {text}"
    );
}

#[test]
fn saving_without_edits_leaves_the_tune_byte_identical() {
    let dir = project_dir("no-op");
    let before = read_tune(&dir);
    let mut project = Project::open(dir.path()).expect("the project opens");

    let outcome = project.save().expect("an unedited project saves");
    assert_eq!(outcome.cells, 0);
    assert_eq!(
        outcome.variables,
        Vec::<String>::new(),
        "saving an unchanged tune rewrites no variables"
    );
    assert_eq!(read_tune(&dir), before);

    // A second save after committing the baseline is still a no-op.
    let outcome = project.save().expect("the project saves again");
    assert_eq!(outcome.cells, 0);
    assert_eq!(read_tune(&dir), before);
}

#[test]
fn saving_with_a_stamp_refreshes_the_bibliography_date() {
    let dir = project_dir("save-stamp");
    let mut project = Project::open(dir.path()).expect("the project opens");

    project
        .save_with_stamp(Some("Sun Nov 22 06:46:58 UTC 2026"))
        .expect("the project saves");
    project
        .save_with_stamp(Some("Mon Nov 23 07:00:00 UTC 2026"))
        .expect("the project saves again");

    let tune = fs::read_to_string(dir.path().join(TUNE_FILE)).expect("the tune is readable");
    assert!(
        tune.contains(r#"<bibliography author="TunerStudio" tuneComment="fixture" writeDate="Mon Nov 23 07:00:00 UTC 2026"/>"#),
        "{tune}"
    );
    assert!(
        !tune.contains("Sun Nov 22"),
        "the second save replaces the earlier date: {tune}"
    );
}

#[test]
fn a_save_records_project_state_inside_the_sidecar() {
    let dir = project_dir("state");
    let definition_bytes =
        fs::read(dir.path().join("projectCfg/mainController.ini")).expect("the INI is readable");
    let mut project = Project::open(dir.path()).expect("the project opens");
    assert_eq!(project.state().connection.last_port, None);

    project.save().expect("the project saves");

    let state = project.state();
    assert_eq!(state.definition.path, "projectCfg/mainController.ini");
    assert_eq!(state.definition.sha256, hex(&sha256(&definition_bytes)));
    assert_eq!(state.definition.signature, "speeduino 202501");

    let reread = ProjectState::read(dir.path())
        .expect("the state is readable")
        .expect("the state was written");
    assert_eq!(&reread, state);
}

#[test]
fn a_corrupt_sidecar_does_not_block_opening_and_heals_on_save() {
    let dir = project_dir("corrupt-state");
    dir.write(".oxtuner/project.json", "{ not json");

    let mut project = Project::open(dir.path()).expect("the project still opens");
    assert!(project.state().connection.last_port.is_none());
    assert_eq!(project.state().definition.signature, "speeduino 202501");

    project.save().expect("the save heals the file");

    let healed = ProjectState::read(dir.path())
        .expect("the state is readable")
        .expect("the state was rewritten");
    assert_eq!(&healed, project.state());
}

#[test]
fn the_device_section_survives_a_reopen() {
    let dir = project_dir("device-state");
    let mut project = Project::open(dir.path()).expect("the project opens");
    project
        .set_connection(Some("/dev/ttyACM0".to_owned()), Some(115_200))
        .expect("the device persists");
    drop(project);

    let reopened = Project::open(dir.path()).expect("the project reopens");
    assert_eq!(
        reopened.state().connection.last_port.as_deref(),
        Some("/dev/ttyACM0")
    );
    assert_eq!(reopened.state().connection.baud_override, Some(115_200));
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
            "project/.oxtuner",
            "project/.oxtuner/project.json",
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
fn a_negative_page_number_holds_pc_variables_and_does_not_gate_the_project() {
    // The project format also writes the PC-variable page as `number="-1"`
    // rather than as an unnumbered page. Those variables are not calibration
    // data, so the page must neither block the open nor count as a tune page.
    let dir = TempDir::new("pc-page-negative");
    dir.write("projectCfg/mainController.ini", MAIN_DEFINITION);
    dir.write("projectCfg/page2.ini", PAGE_TWO);
    let tune = TUNE.replace(
        r#"    <page>
        <pcVariable digits="0" name="rpmhigh" units="rpm">12000.0</pcVariable>
    </page>"#,
        r#"    <page number="-1">
        <pcVariable digits="0" name="rpmhigh" units="rpm">12000.0</pcVariable>
        <pcVariable digits="0" name="AUXin00Alias">"Aux0"</pcVariable>
    </page>"#,
    );
    dir.write(TUNE_FILE, &tune);

    let project = Project::open(dir.path()).expect("the tune opens");
    assert!(
        !project.is_read_only(),
        "a PC-variable page must not gate the project: {:?}",
        project.report().blockers
    );
}

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
    assert!(project.state().connection.last_port.is_none());

    project.tune_mut().set_user("tempUnits", 4.0).unwrap();
    let outcome = project.save().expect("the edit saves");
    assert_eq!(outcome.variables, ["tempUnits"]);
    assert!(project.state().connection.last_port.is_none());
    assert!(!dir.path().join(".oxtuner").exists());
}

#[test]
fn a_standalone_tune_resolves_its_includes() {
    let dir = TempDir::new("standalone-include");
    dir.write(
        "standalone.ini",
        "[MegaTune]\nsignature = \"speeduino 202501\"\n[Constants]\nnPages = 1\npageSize = 64\npage = 1\n#include extra.inc\n",
    );
    dir.write(
        "extra.inc",
        "fromInclude = scalar, U08, 0, \"\", 1, 0, 0, 255, 0\n",
    );
    dir.write("tune.msq", STANDALONE_TUNE);

    let project = Project::open_standalone(
        dir.path().join("tune.msq"),
        dir.path().join("standalone.ini"),
    )
    .expect("the tune opens");

    assert!(
        project.definition().constant("fromInclude").is_some(),
        "a standalone open must resolve its includes"
    );
}

#[cfg(unix)]
#[test]
fn a_standalone_tune_resolves_includes_through_a_symlinked_base() {
    use std::os::unix::fs::symlink;

    let real = TempDir::new("standalone-include-real");
    real.write(
        "standalone.ini",
        "[MegaTune]\nsignature = \"speeduino 202501\"\n[Constants]\nnPages = 1\npageSize = 64\npage = 1\n#include extra.inc\n",
    );
    real.write(
        "extra.inc",
        "fromInclude = scalar, U08, 0, \"\", 1, 0, 0, 255, 0\n",
    );
    real.write("tune.msq", STANDALONE_TUNE);

    let alias = TempDir::new("standalone-include-alias");
    let alias_path = alias.path().join("via-symlink");
    symlink(real.path(), &alias_path).expect("the alias is creatable");

    let project = Project::open_standalone(
        alias_path.join("tune.msq"),
        alias_path.join("standalone.ini"),
    )
    .expect("the tune opens");

    assert!(
        project.definition().constant("fromInclude").is_some(),
        "the include base must be canonicalized before its paths are compared"
    );
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
    assert!(
        project
            .report()
            .warnings
            .iter()
            .all(|issue| !issue.message.contains("presentation attribute")),
        "every presentation attribute must resolve on the reference project: {:?}",
        project.report().warnings
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
/// Set `OXTUNER_REFERENCE_PROJECT` to a `TunerStudio` project directory and run
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
    assert!(
        project
            .report()
            .warnings
            .iter()
            .all(|issue| !issue.message.contains("presentation attribute")),
        "every presentation attribute must resolve on the reference project: {:?}",
        project.report().warnings
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
fn a_project_properties_file_selects_definition_flags() {
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
</msq>
"#;

    let dir = TempDir::new("project-properties");
    dir.write("projectCfg/mainController.ini", DEFINITION);
    dir.write("projectCfg/project.properties", "ecuSettings=CELSIUS|\n");
    dir.write(TUNE_FILE, TUNE);

    let project = Project::open(dir.path()).expect("the project opens");

    assert!(project.settings().is_active("CELSIUS"));
    assert!(!project.is_read_only(), "{:?}", project.report().blockers);
    assert_close(project.tune().scalar_user("tempUnits").unwrap(), 1.0);
}

#[test]
fn a_hand_edited_state_file_is_applied_on_open() {
    let dir = project_dir("state-hand-edited");
    dir.write(
        ".oxtuner/project.json",
        r#"{
            "schema_version": 1,
            "connection": { "last_port": "/dev/ttyACM9", "baud_override": 57600 },
            "layout": {
                "primary_sidebar_width": 321.0,
                "secondary_sidebar_width": 222.0,
                "bottom_panel_height": 199.0,
                "primary_sidebar_visible": false,
                "secondary_sidebar_visible": true,
                "bottom_panel_visible": false,
                "table_low_at_top": true
            }
        }"#,
    );

    let project = Project::open(dir.path()).expect("the project opens");

    assert_eq!(
        project.state().connection.last_port.as_deref(),
        Some("/dev/ttyACM9")
    );
    assert_eq!(project.state().connection.baud_override, Some(57_600));
    assert_eq!(project.state().layout.primary_sidebar_width, 321.0);
    assert!(!project.state().layout.primary_sidebar_visible);
    assert!(!project.state().layout.bottom_panel_visible);
    assert!(project.state().layout.table_low_at_top);
}

#[test]
fn the_tune_snapshot_beats_ecu_settings_on_a_duplicate() {
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
    // The project file turns CELSIUS on, the tune snapshot turns it off.
    const TUNE: &str = r#"<msq>
    <versionInfo fileFormat="5.0" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="64"/>
    <settings>
        <setting name="CELSIUS_OFF"/>
    </settings>
</msq>
"#;

    let dir = TempDir::new("settings-precedence");
    dir.write("projectCfg/mainController.ini", DEFINITION);
    dir.write("projectCfg/project.properties", "ecuSettings=CELSIUS|\n");
    dir.write(TUNE_FILE, TUNE);

    let project = Project::open(dir.path()).expect("the project opens");
    assert!(
        !project.settings().is_active("CELSIUS"),
        "the tune snapshot is the settings context and wins on a duplicate"
    );
}

#[test]
fn the_project_pc_values_win_over_the_tune_page() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
a = scalar, U08, 0, "", 1, 0, 0, 255, 0

[PcVariables]
rpmhigh = scalar, U16, "rpm", 1, 0, 0, 30000, 0
"#;
    const TUNE: &str = r#"<msq>
    <versionInfo fileFormat="5.0" nPages="1" signature="speeduino 202501"/>
    <page>
        <pcVariable name="rpmhigh">12000.0</pcVariable>
    </page>
    <page number="0" size="64">
        <constant name="a">1.0</constant>
    </page>
</msq>
"#;
    const PC_VALUES: &str = r#"<msq>
    <versionInfo fileFormat="4.0" signature="speeduino 202501"/>
    <page number="-1">
        <pcVariable name="rpmhigh">9000</pcVariable>
    </page>
</msq>
"#;

    let dir = TempDir::new("project-pc-values");
    dir.write("projectCfg/mainController.ini", DEFINITION);
    dir.write("projectCfg/pcVariableValues.msq", PC_VALUES);
    dir.write(TUNE_FILE, TUNE);

    let mut project = Project::open(dir.path()).expect("the project opens");

    assert_eq!(
        project.tune().scalar_user("rpmhigh").unwrap(),
        9000.0,
        "the project file carries the application-level value"
    );

    let before = read_tune(&dir);
    project.save().expect("an unedited project saves");
    assert_eq!(
        read_tune(&dir),
        before,
        "reading the project PC values does not rewrite the tune"
    );
}

#[test]
fn a_bare_setting_survives_a_no_op_save() {
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
    // The format also writes `<setting name="X"/>` with no `value` attribute.
    const TUNE: &str = r#"<msq>
    <versionInfo fileFormat="5.0" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="64">
        <constant name="tempUnits">1.0</constant>
    </page>
    <settings Comment="These setting are only used if this msq is opened without a project.">
        <setting name="enablehardware_test" />
        <setting name="CAN_COMMANDS_OFF" />
        <setting name="CELSIUS" />
    </settings>
</msq>
"#;

    let dir = TempDir::new("bare-settings");
    dir.write("projectCfg/mainController.ini", DEFINITION);
    dir.write(TUNE_FILE, TUNE);
    let before = read_tune(&dir);

    let project = Project::open(dir.path()).expect("the project opens");
    assert!(project.settings().is_active("CELSIUS"));
    assert!(!project.settings().is_active("CAN_COMMANDS"));
    assert!(!project.is_read_only(), "{:?}", project.report().blockers);

    let mut project = project;
    let outcome = project.save().expect("an unedited project saves");

    assert_eq!(outcome.cells, 0);
    assert_eq!(
        read_tune(&dir),
        before,
        "the bare setting form must survive a no-op save"
    );
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
tag = string, ASCII, 8
"#;
    const TUNE: &str = r#"<msq>
    <versionInfo fileFormat="5.0" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="64">
        <constant name="a">1.0</constant>
        <constant name="mode">"ONE"</constant>
        <constant name="tag">Ok</constant>
    </page>
</msq>
"#;

    let dir = TempDir::new("failed-save");
    dir.write("projectCfg/mainController.ini", DEFINITION);
    dir.write(TUNE_FILE, TUNE);
    let before = read_tune(&dir);

    let mut project = Project::open(dir.path()).expect("the project opens");
    project.tune_mut().set_user("a", 5.0).unwrap();
    // `a` is formatted first; the text fails after it, because ISO-8859-1 has
    // no byte for this character.
    project.tune_mut().set_text("tag", "🚗").unwrap();

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

#[test]
fn creating_a_project_scaffolds_the_reference_layout() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
reqFuel = scalar, U08, 0, "ms", 0.1, 0, 0, 25.5, 1

[PcVariables]
rpmhigh = scalar, U16, 0, "rpm", 1, 0, 0, 20000, 0
"#;

    let sandbox = TempDir::new("create");
    let request = CreateRequest {
        parent: sandbox.path().to_path_buf(),
        name: "Roadster".to_owned(),
        ini_path: sandbox.definition(DEFINITION),
        expected_signature: Some("speeduino 202501".to_owned()),
        settings: Vec::new(),
        connection: None,
        layout: ProjectLayout::default(),
        write_stamp: None,
    };
    let project = Project::create(&request).expect("creation succeeds");

    let root = sandbox.path().join("Roadster");
    assert!(root.join(TUNE_FILE).is_file());
    assert!(root.join("projectCfg/mainController.ini").is_file());
    for dir in ["dashboard", "TuneView"] {
        assert!(root.join(dir).is_dir(), "{dir} is scaffolded");
    }
    assert!(root.join(".oxtuner/project.json").is_file());

    let properties =
        fs::read_to_string(root.join("projectCfg/project.properties")).expect("properties exist");
    assert!(properties.contains("projectName=Roadster"));
    assert!(properties.contains("speeduino 202501"));
    assert!(properties.contains("ecuConfigFile=mainController.ini"));
    assert!(properties.contains("firmwareDescription=speeduino 202501"));
    assert!(
        properties.contains("ecuSettings=\n"),
        "no group selected yet: {properties}"
    );
    assert!(
        root.join("projectCfg/project.properties.bkup").is_file(),
        "TunerStudio expects a backup copy beside the properties"
    );
    assert!(
        !properties.contains("signature="),
        "the TS key is firmwareDescription: {properties}"
    );

    assert!(!project.is_read_only(), "{:?}", project.report().blockers);
    assert_close(project.tune().scalar_user("reqFuel").unwrap(), 0.0);
    assert_eq!(
        project.state().definition.path,
        "projectCfg/mainController.ini"
    );
    drop(project);

    let reopened = Project::open(&root).expect("the created project reopens");
    assert!(!reopened.is_read_only(), "{:?}", reopened.report().blockers);
}

#[test]
fn a_one_row_table_keeps_its_declared_shape() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
rowWise = array, U08, 0, [4x1], "", 1, 0, 0, 255, 0
"#;

    let sandbox = TempDir::new("create-row-table");
    let request = CreateRequest {
        parent: sandbox.path().to_path_buf(),
        name: "RowTable".to_owned(),
        ini_path: sandbox.definition(DEFINITION),
        expected_signature: None,
        settings: Vec::new(),
        connection: None,
        layout: ProjectLayout::default(),
        write_stamp: None,
    };
    let project = Project::prepare(&request)
        .expect("a one-row table is a legal shape")
        .create(None, WritePolicy::Full, None)
        .expect("creation succeeds");

    let tune = fs::read_to_string(project.tune_path()).expect("the tune is readable");
    assert!(
        tune.contains(r#"cols="4" digits="0" name="rowWise" rows="1""#),
        "a declared [4x1] table keeps its shape: {tune}"
    );
    assert!(!project.is_read_only(), "{:?}", project.report().blockers);
}

#[test]
fn creation_writes_the_reference_properties_header_and_rotates_the_backup() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[SettingGroups]
settingGroup = enablehardware_test, "Enable Hardware Test Page"
settingGroup = resetcontrol_group, "Reset Control Features"
settingOption = resetcontrol_standard, "Basic Options Only"

[Constants]
nPages = 1
pageSize = 16
page = 1
#if LAMBDA
lambda_value = scalar, U08, 0, "", 1, 0, 0, 255, 0
#else
afr_value = scalar, U08, 0, "", 1, 0, 0, 255, 0
#endif
#if CELSIUS
celsius_value = scalar, U08, 1, "", 1, 0, 0, 255, 0
#else
fahrenheit_value = scalar, U08, 1, "", 1, 0, 0, 255, 0
#endif
"#;

    let sandbox = TempDir::new("create-metadata");
    let request = CreateRequest {
        parent: sandbox.path().to_path_buf(),
        name: "Metadata".to_owned(),
        ini_path: sandbox.definition(DEFINITION),
        expected_signature: None,
        settings: vec!["resetcontrol_standard".to_owned()],
        connection: Some(ConnectionSeed {
            port: "COM6".to_owned(),
            baud: 115_200,
        }),
        layout: ProjectLayout::default(),
        write_stamp: Some("Sun Nov 22 06:46:58 UTC 2026".to_owned()),
    };
    let project = Project::create(&request).expect("creation succeeds");
    let root = project.root().to_path_buf();
    drop(project);

    let properties =
        fs::read_to_string(root.join("projectCfg/project.properties")).expect("properties");
    assert!(
        properties.starts_with(
            "#Project Attributes.\n#Ox Tuner\n#Last Saved on: Sun Nov 22 06:46:58 UTC 2026\n#Sun Nov 22 06:46:58 UTC 2026\n"
        ),
        "{properties}"
    );
    for key in [
        "projectName=Metadata",
        "commPort=COM6",
        "baudRate=115200",
        "canId=-1",
        "ecuUid=",
        "firmwareDescription=speeduino 202501",
        "ecuSettings=resetcontrol_standard|enablehardware_test_OFF|AFR|FAHRENHEIT|",
        "useCommonDashboardDir=false",
        "useCommonTuneViewDir=false",
        "lastDisplayedTuneFile=",
    ] {
        assert!(properties.contains(key), "{key} missing: {properties}");
    }

    let backup =
        fs::read_to_string(root.join("projectCfg/project.properties.bkup")).expect("the backup");
    assert!(
        backup.contains("useCommonDashboardDir=false"),
        "the first stage already knows the dashboard choice: {backup}"
    );
    assert!(backup.contains("commPort=COM6"), "{backup}");
    assert!(backup.contains("ecuSettings="), "{backup}");
    for later in [
        "canId=",
        "ecuUid=",
        "firmwareDescription=",
        "lastDisplayedTuneFile=",
    ] {
        assert!(
            !backup.contains(later),
            "the backup is the first-stage file, so {later} is not in it: {backup}"
        );
    }

    let tune = fs::read_to_string(root.join(TUNE_FILE)).expect("the tune");
    assert!(
        tune.contains(
            r#"<bibliography author="Ox Tuner" tuneComment="" writeDate="Sun Nov 22 06:46:58 UTC 2026"/>"#
        ),
        "{tune}"
    );
    for flag in [
        "AFR",
        "FAHRENHEIT",
        "enablehardware_test_OFF",
        "resetcontrol_standard",
    ] {
        assert!(
            tune.contains(&format!(r#"<setting name="{flag}" value="{flag}"/>"#)),
            "{flag} missing: {tune}"
        );
    }
}

#[test]
fn creation_writes_a_unique_v4_ecu_uid() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 16
page = 1
value = scalar, U08, 0, "", 1, 0, 0, 255, 0
"#;

    let sandbox = TempDir::new("create-ecu-uid");
    let mut uids = Vec::new();
    for name in ["UidOne", "UidTwo"] {
        let request = CreateRequest {
            parent: sandbox.path().to_path_buf(),
            name: name.to_owned(),
            ini_path: sandbox.definition(DEFINITION),
            expected_signature: None,
            settings: Vec::new(),
            connection: None,
            layout: ProjectLayout::default(),
            write_stamp: None,
        };
        let project = Project::create(&request).expect("creation succeeds");
        let properties = fs::read_to_string(project.root().join("projectCfg/project.properties"))
            .expect("properties");

        let uid = properties
            .lines()
            .find_map(|line| line.strip_prefix("ecuUid="))
            .unwrap_or_else(|| panic!("no ecuUid line: {properties}"));
        let parsed = Uuid::parse_str(uid).expect("the uid is a UUID");
        assert_eq!(
            parsed.get_version_num(),
            4,
            "a project identity is a v4 UUID: {uid}"
        );

        uids.push(uid.to_owned());
    }

    assert_ne!(uids[0], uids[1], "each project gets its own identity");
}

#[test]
fn creation_without_a_stamp_writes_an_undated_header() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 16
page = 1
value = scalar, U08, 0, "", 1, 0, 0, 255, 0
"#;

    let sandbox = TempDir::new("create-undated");
    let request = CreateRequest {
        parent: sandbox.path().to_path_buf(),
        name: "Undated".to_owned(),
        ini_path: sandbox.definition(DEFINITION),
        expected_signature: None,
        settings: Vec::new(),
        connection: None,
        layout: ProjectLayout::default(),
        write_stamp: None,
    };
    let project = Project::create(&request).expect("creation succeeds");
    let root = project.root().to_path_buf();
    drop(project);

    let properties =
        fs::read_to_string(root.join("projectCfg/project.properties")).expect("properties");
    assert!(
        properties.starts_with("#Project Attributes.\n#Ox Tuner\nprojectName=Undated\n"),
        "{properties}"
    );
    assert!(!properties.contains("Last Saved on:"), "{properties}");

    let tune = fs::read_to_string(root.join(TUNE_FILE)).expect("the tune");
    assert!(
        tune.contains(r#"<bibliography author="Ox Tuner" tuneComment=""/>"#),
        "{tune}"
    );
    assert!(!tune.contains("writeDate="), "{tune}");
}

#[test]
fn a_controller_version_text_fills_the_firmware_fields() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"
versionInfo = "S"

[Constants]
nPages = 1
pageSize = 16
page = 1
value = scalar, U08, 0, "", 1, 0, 0, 255, 0
"#;

    let sandbox = TempDir::new("create-version");
    let request = CreateRequest {
        parent: sandbox.path().to_path_buf(),
        name: "Versioned".to_owned(),
        ini_path: sandbox.definition(DEFINITION),
        expected_signature: None,
        settings: Vec::new(),
        connection: None,
        layout: ProjectLayout::default(),
        write_stamp: None,
    };
    let project = Project::prepare(&request)
        .expect("the request prepares")
        .create(None, WritePolicy::Full, Some("Speeduino 2025.01.7"))
        .expect("creation succeeds");
    let root = project.root().to_path_buf();
    drop(project);

    let tune = fs::read_to_string(root.join(TUNE_FILE)).expect("the tune");
    assert!(
        tune.contains(r#"firmwareInfo="Speeduino 2025.01.7""#),
        "{tune}"
    );
    assert!(
        tune.contains(r#"signature="speeduino 202501""#),
        "the signature stays the definition's: {tune}"
    );
    let properties =
        fs::read_to_string(root.join("projectCfg/project.properties")).expect("properties");
    assert!(
        properties.contains("firmwareDescription=Speeduino 2025.01.7"),
        "{properties}"
    );
}

#[test]
fn a_created_tune_starts_from_the_declared_array_defaults() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
axis = array, U08, 0, [3], "", 1, 0, 0, 255, 0

[ConstantsExtensions]
defaultValue = axis, 10 20 30
"#;

    let sandbox = TempDir::new("create-array-defaults");
    let request = CreateRequest {
        parent: sandbox.path().to_path_buf(),
        name: "ArrayDefaults".to_owned(),
        ini_path: sandbox.definition(DEFINITION),
        expected_signature: None,
        settings: Vec::new(),
        connection: None,
        layout: ProjectLayout::default(),
        write_stamp: None,
    };
    let project = Project::create(&request).expect("creation succeeds");

    let values = project
        .tune()
        .value("axis")
        .expect("the axis is in the tune");
    assert_eq!(values.number(0), Some(10.0));
    assert_eq!(values.number(1), Some(20.0));
    assert_eq!(values.number(2), Some(30.0));
}

#[test]
fn a_raw_array_default_wins_over_the_user_default() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
axis = array, U08, 0, [3], "", 1, 0, 0, 255, 0

[ConstantsExtensions]
defaultValue = axis, 10 20 30
rawValue = axis, 1 2 3
"#;

    let sandbox = TempDir::new("create-raw-defaults");
    let request = CreateRequest {
        parent: sandbox.path().to_path_buf(),
        name: "RawDefaults".to_owned(),
        ini_path: sandbox.definition(DEFINITION),
        expected_signature: None,
        settings: Vec::new(),
        connection: None,
        layout: ProjectLayout::default(),
        write_stamp: None,
    };
    let project = Project::create(&request).expect("creation succeeds");

    let values = project
        .tune()
        .value("axis")
        .expect("the axis is in the tune");
    assert_eq!(
        (values.number(0), values.number(1), values.number(2)),
        (Some(1.0), Some(2.0), Some(3.0)),
        "rawValue wins over defaultValue, as it does for a scalar"
    );
}

#[test]
fn a_new_project_with_two_views_of_one_region_opens_writable() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 512
page = 1
lambdaTable = array, U08, 0, [16x16], "Lambda", 0.0068, 0.0, 0.00, 2.00, 3
afrTable = array, U08, lastOffset, [16x16], "AFR", 0.1, 0.0, 7, 25.5, 1
"#;

    let sandbox = TempDir::new("create-shared-views");
    let request = CreateRequest {
        parent: sandbox.path().to_path_buf(),
        name: "SharedViews".to_owned(),
        ini_path: sandbox.definition(DEFINITION),
        expected_signature: None,
        settings: Vec::new(),
        connection: None,
        layout: ProjectLayout::default(),
        write_stamp: None,
    };
    let project = Project::create(&request).expect("two views of one region share one default");
    assert!(!project.is_read_only(), "{:?}", project.report().blockers);

    // A default-only array stays implicit, and both views must carry the same
    // bytes when the file omits them.
    let tune = fs::read_to_string(project.root().join(TUNE_FILE)).expect("the tune");
    assert!(
        !tune.contains("lambdaTable") && !tune.contains("afrTable"),
        "neither view is materialized: {tune}"
    );
    assert_eq!(
        project.tune().raw_at("lambdaTable", 0),
        project.tune().raw_at("afrTable", 0),
        "one byte region holds one value"
    );

    let reopened = Project::open(project.root()).expect("reopens");
    assert!(!reopened.is_read_only(), "{:?}", reopened.report().blockers);
}

#[test]
fn saving_keeps_the_cell_indentation_the_file_uses() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 16
page = 1
table = array, U08, 0, [2x2], "", 1, 0, 0, 255, 0
"#;
    const TUNE: &str = "<msq>\n    <versionInfo fileFormat=\"5.0\" nPages=\"1\" signature=\"speeduino 202501\"/>\n    <page number=\"0\" size=\"16\">\n        <constant cols=\"2\" rows=\"2\" name=\"table\">\n         1.0 2.0 \n         3.0 4.0 \n      </constant>\n    </page>\n</msq>\n";

    let dir = TempDir::new("save-indent");
    dir.write("projectCfg/mainController.ini", DEFINITION);
    dir.write(TUNE_FILE, TUNE);

    let mut project = Project::open(dir.path()).expect("the project opens");
    project
        .tune_mut()
        .set_cell_user("table", 0, 1, 7.0)
        .expect("the cell is editable");
    project.save().expect("the tune saves");

    let saved = tune_text(&dir);
    assert!(
        saved.contains("\n         1.0 7.0\n         3.0 4.0\n      </constant>"),
        "an edited array keeps the file's own indentation: {saved}"
    );
}

#[test]
fn a_single_line_array_is_written_in_the_entry_layout() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 16
page = 1
table = array, U08, 0, [2x2], "", 1, 0, 0, 255, 0
"#;
    const TUNE: &str = "<msq>\n    <versionInfo fileFormat=\"5.0\" nPages=\"1\" signature=\"speeduino 202501\"/>\n    <page number=\"0\" size=\"16\">\n        <constant cols=\"2\" rows=\"2\" name=\"table\">1.0 2.0 3.0 4.0</constant>\n    </page>\n</msq>\n";

    let dir = TempDir::new("save-inline");
    dir.write("projectCfg/mainController.ini", DEFINITION);
    dir.write(TUNE_FILE, TUNE);

    let mut project = Project::open(dir.path()).expect("the project opens");
    project
        .tune_mut()
        .set_cell_user("table", 0, 1, 7.0)
        .expect("the cell is editable");
    project.save().expect("the tune saves");

    let saved = tune_text(&dir);
    assert!(
        saved.contains("\n            1.0 7.0\n            3.0 4.0\n        </constant>"),
        "a one-line entry has no indentation to follow, so the written layout applies: {saved}"
    );
}

#[test]
fn saving_a_crlf_file_keeps_its_line_endings_and_cell_indent() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 16
page = 1
table = array, U08, 0, [2x2], "", 1, 0, 0, 255, 0
"#;
    const TUNE: &str = "<msq>\r\n    <versionInfo fileFormat=\"5.0\" nPages=\"1\" signature=\"speeduino 202501\"/>\r\n    <page number=\"0\" size=\"16\">\r\n        <constant cols=\"2\" rows=\"2\" name=\"table\">\r\n         1.0 2.0 \r\n         3.0 4.0 \r\n      </constant>\r\n    </page>\r\n</msq>\r\n";

    let dir = TempDir::new("save-crlf-indent");
    dir.write("projectCfg/mainController.ini", DEFINITION);
    dir.write(TUNE_FILE, TUNE);

    let mut project = Project::open(dir.path()).expect("the project opens");
    project
        .tune_mut()
        .set_cell_user("table", 0, 1, 7.0)
        .expect("the cell is editable");
    project.save().expect("the tune saves");

    let saved = tune_text(&dir);
    assert!(
        saved.contains("\r\n         1.0 7.0\r\n         3.0 4.0\r\n      </constant>"),
        "the edited cells keep the file's indentation and line ending: {saved:?}"
    );
    assert_eq!(
        saved.matches('\n').count(),
        saved.matches("\r\n").count(),
        "no line ending changes style: {saved:?}"
    );
}

#[test]
fn an_appended_entry_follows_a_flat_page() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 16
page = 1
first = array, U08, 0, [2x2], "", 1, 0, 0, 255, 0
second = array, U08, 4, [2x2], "", 1, 0, 0, 255, 0
"#;
    const TUNE: &str = "<msq>\n<versionInfo fileFormat=\"5.0\" nPages=\"1\" signature=\"speeduino 202501\"/>\n<page number=\"0\" size=\"16\">\n<constant cols=\"2\" rows=\"2\" name=\"first\">1.0 2.0 3.0 4.0</constant>\n</page>\n</msq>\n";

    let dir = TempDir::new("append-flat");
    dir.write("projectCfg/mainController.ini", DEFINITION);
    dir.write(TUNE_FILE, TUNE);

    let mut project = Project::open(dir.path()).expect("the project opens");
    project
        .tune_mut()
        .set_cell_user("second", 0, 0, 9.0)
        .expect("the cell is editable");
    project.save().expect("the tune saves");

    let saved = tune_text(&dir);
    assert!(
        saved.contains(
            "\n<constant name=\"second\">\n    9.0 0.0\n    0.0 0.0\n</constant>\n</page>"
        ),
        "an appended entry follows the page's column-zero layout: {saved}"
    );
}

#[test]
fn a_project_created_from_an_image_keeps_every_byte() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
lowView = array, U08, 0, [2x2], "Lambda", 0.0068, 0, 0.00, 2.00, 3
highView = array, U08, lastOffset, [2x2], "AFR", 0.1, 0, 7, 25.5, 1
belowMin = scalar, U08, 4, "", 1, 0, 100, 255, 0
"#;

    let sandbox = TempDir::new("create-image-roundtrip");
    let definition =
        oxtuner_ini::parse_str(DEFINITION, &oxtuner_ini::SettingsContext::new()).expect("parses");
    let mut seed = oxtuner_tune::Tune::from_definition(&definition);
    for (name, values) in [
        ("lowView", [0.0, 37.0, 200.0, 255.0]),
        ("highView", [0.0, 37.0, 200.0, 255.0]),
    ] {
        for (index, raw) in values.into_iter().enumerate() {
            seed.set_image_raw(name, index, raw)
                .expect("the cell exists");
        }
    }
    seed.set_image_raw("belowMin", 0, 0.0)
        .expect("the cell exists");

    let request = CreateRequest {
        parent: sandbox.path().to_path_buf(),
        name: "ImageRoundTrip".to_owned(),
        ini_path: sandbox.definition(DEFINITION),
        expected_signature: None,
        settings: Vec::new(),
        connection: None,
        layout: ProjectLayout::default(),
        write_stamp: None,
    };
    let project = Project::prepare(&request)
        .expect("the request prepares")
        .create(Some(&seed), WritePolicy::Full, None)
        .expect("creation succeeds");

    let tune = fs::read_to_string(project.tune_path()).expect("the tune is readable");
    assert!(
        tune.contains("name=\"belowMin\">0.0</constant>"),
        "a value below the declared minimum is written as it is: {tune}"
    );
    assert!(
        tune.contains("name=\"highView\""),
        "both views of one region are written from the same bytes: {tune}"
    );

    for spec in seed.layout().variables() {
        for index in 0..spec.element_count() {
            assert_eq!(
                seed.raw_at(&spec.name, index).expect("the seed has it"),
                project
                    .tune()
                    .raw_at(&spec.name, index)
                    .expect("the project has it"),
                "`{}`[{index}] changed across create and open",
                spec.name
            );
        }
    }
}

#[test]
fn a_text_that_reads_below_the_bound_is_kept_and_reported() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
ego = scalar, U08, 0, "", 0.006802721088435375, 0, 0.4761904761904762, 1.7006802721088436, 3
"#;
    // The text carries three decimals, so it reads below the minimum. The value
    // is kept: the range bounds an edit, not what a file states.
    const TUNE: &str = r#"<msq>
    <versionInfo fileFormat="5.0" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="64">
        <constant name="ego">0.476</constant>
    </page>
</msq>
"#;

    let dir = TempDir::new("rounded-clamp");
    dir.write("projectCfg/mainController.ini", DEFINITION);
    dir.write(TUNE_FILE, TUNE);

    let project = Project::open(dir.path()).expect("the project opens");

    let out_of_range: Vec<&str> = project
        .report()
        .warnings
        .iter()
        .filter(|issue| issue.kind == IssueKind::ValueOutOfRange)
        .map(|issue| issue.message.as_str())
        .collect();
    assert_eq!(out_of_range.len(), 1, "{:?}", project.report().warnings);
    assert!(
        out_of_range[0].contains("0.476"),
        "the report names the value the file states: {out_of_range:?}"
    );
    assert_eq!(
        project
            .tune()
            .value("ego")
            .and_then(|values| values.number(0)),
        Some(70.0),
        "70 raw is the 0.476 the file states, not the 100 a clamp would store"
    );
}

#[test]
fn an_array_the_file_omits_opens_on_its_declared_default() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
axis = array, U08, 0, [3], "", 1, 0, 0, 255, 0

[ConstantsExtensions]
defaultValue = axis, 10 20 30
"#;
    const TUNE: &str = r#"<msq>
    <versionInfo fileFormat="5.0" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="64"/>
</msq>
"#;

    let dir = TempDir::new("array-default-open");
    dir.write("projectCfg/mainController.ini", DEFINITION);
    dir.write(TUNE_FILE, TUNE);

    let project = Project::open(dir.path()).expect("the project opens");

    let values = project
        .tune()
        .value("axis")
        .expect("the axis is in the tune");
    assert_eq!(
        (values.number(0), values.number(1), values.number(2)),
        (Some(10.0), Some(20.0), Some(30.0)),
        "an omitted array starts from the definition's declared values"
    );
}

#[test]
fn a_declared_default_at_the_bound_keeps_its_text() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
p = scalar, F32, 0, "", 0.145038, 0, 7.25, 101526, 0

[ConstantsExtensions]
defaultValue = p, 7.25
"#;

    let sandbox = TempDir::new("create-bound-rounding");
    let request = CreateRequest {
        parent: sandbox.path().to_path_buf(),
        name: "BoundRounding".to_owned(),
        ini_path: sandbox.definition(DEFINITION),
        expected_signature: None,
        settings: Vec::new(),
        connection: None,
        layout: ProjectLayout::default(),
        write_stamp: None,
    };
    let project = Project::create(&request).expect("creation succeeds");

    let reopened = Project::open(project.root()).expect("the project reopens");
    assert!(
        reopened
            .report()
            .warnings
            .iter()
            .all(|issue| issue.kind != IssueKind::ValueOutOfRange),
        "the declared default is written and read back as declared: {:?}",
        reopened.report().warnings
    );
    let raw = reopened
        .tune()
        .value("p")
        .and_then(|values| values.number(0))
        .expect("the value is there");
    assert!(
        (raw - 7.25 / 0.145_038).abs() < 1e-9,
        "the declared 7.25 survives the round trip: {raw}"
    );
}

#[test]
fn a_range_value_and_a_storage_overflow_are_each_reported() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
axis = array, S16, 0, [2], "", 1, 0, 0, 100000, 0
"#;
    const TUNE: &str = r#"<msq>
    <versionInfo fileFormat="5.0" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="64">
        <constant name="axis">-5.0 70000.0</constant>
    </page>
</msq>
"#;

    let dir = TempDir::new("cell-warnings");
    dir.write("projectCfg/mainController.ini", DEFINITION);
    dir.write(TUNE_FILE, TUNE);

    let project = Project::open(dir.path()).expect("the project opens");
    let kinds: Vec<IssueKind> = project
        .report()
        .warnings
        .iter()
        .map(|issue| issue.kind)
        .collect();

    assert_eq!(
        project.tune().raw_at("axis", 0).unwrap(),
        -5.0,
        "a value inside the storage type is kept whatever the range says"
    );
    assert_eq!(
        project.tune().raw_at("axis", 1).unwrap(),
        32_767.0,
        "a value past the storage type is clamped to it"
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == IssueKind::ValueOutOfRange)
            .count(),
        2,
        "one range report and one storage report: {:?}",
        project.report().warnings
    );
    assert!(
        project
            .report()
            .warnings
            .iter()
            .any(|issue| issue.message.contains("kept as written")),
        "the out-of-range cell is kept: {:?}",
        project.report().warnings
    );
    assert!(
        project
            .report()
            .warnings
            .iter()
            .any(|issue| issue.message.contains("`axis`[1]")),
        "the storage warning names the cell: {:?}",
        project.report().warnings
    );
}

#[test]
fn a_later_cell_that_cannot_convert_leaves_every_cell_at_its_default() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
big = array, U08, 0, [2], "", 1e-9, 0, -1e308, 1e308, 0
"#;
    const TUNE: &str = r#"<msq>
    <versionInfo fileFormat="5.0" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="64">
        <constant name="big">1 1e308</constant>
    </page>
</msq>
"#;

    let dir = TempDir::new("later-cell");
    dir.write("projectCfg/mainController.ini", DEFINITION);
    dir.write(TUNE_FILE, TUNE);

    let project = Project::open(dir.path()).expect("the project opens");

    assert_eq!(
        project.tune().raw_at("big", 0).expect("cell 0"),
        0.0,
        "the first cell is not written before the second is refused"
    );
    assert!(
        project
            .report()
            .warnings
            .iter()
            .any(|issue| issue.message.contains("`big`[1]")),
        "the refusal names the cell: {:?}",
        project.report().warnings
    );
}

#[test]
fn a_bad_token_in_an_array_keeps_every_cell_at_its_default() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
axis = array, U08, 0, [2], "", 1, 0, 0, 255, 0
"#;
    const TUNE: &str = r#"<msq>
    <versionInfo fileFormat="5.0" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="64">
        <constant name="axis">1.0 nope</constant>
    </page>
</msq>
"#;

    let dir = TempDir::new("partial-array");
    dir.write("projectCfg/mainController.ini", DEFINITION);
    dir.write(TUNE_FILE, TUNE);

    let project = Project::open(dir.path()).expect("the project opens");

    let values = project
        .tune()
        .value("axis")
        .expect("the axis is in the tune");
    assert_eq!(
        (values.number(0), values.number(1)),
        (Some(0.0), Some(0.0)),
        "a bad token leaves no cell written from the file"
    );
    assert_eq!(
        project
            .report()
            .warnings
            .iter()
            .filter(|issue| issue.kind == IssueKind::ValueSkipped)
            .count(),
        1,
        "the value is skipped once, not half-applied"
    );
}

#[test]
fn a_non_finite_value_is_skipped_and_keeps_the_default() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
p = scalar, U08, 0, "", 1, 0, 0, 255, 0
mode = bits, U08, 1, [0:1], "Off", "On"
"#;
    const TUNE: &str = r#"<msq>
    <versionInfo fileFormat="5.0" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="64">
        <constant name="p">NaN</constant>
        <constant name="mode">NaN</constant>
    </page>
</msq>
"#;

    let dir = TempDir::new("non-finite");
    dir.write("projectCfg/mainController.ini", DEFINITION);
    dir.write(TUNE_FILE, TUNE);

    let project = Project::open(dir.path()).expect("the project opens");

    let skipped = project
        .report()
        .warnings
        .iter()
        .filter(|issue| issue.kind == IssueKind::ValueSkipped)
        .count();
    assert_eq!(skipped, 2, "scalar and bit values are both refused");
    assert_eq!(
        project
            .tune()
            .value("p")
            .and_then(|values| values.number(0)),
        Some(0.0),
        "the scalar keeps the definition default"
    );
    assert_eq!(
        project
            .tune()
            .value("mode")
            .and_then(|values| values.number(0)),
        Some(0.0),
        "the bit field keeps the definition default"
    );
}

#[test]
fn a_rejected_scaffold_names_itself() {
    let error = ProjectError::ScaffoldRejected {
        reasons: vec!["`pageSize` disagrees".to_owned()],
    };

    assert_eq!(error.kind(), "scaffold-rejected");
    assert!(
        error.to_string().contains("`pageSize` disagrees"),
        "{error}"
    );
}

#[test]
fn a_non_ascii_project_name_is_escaped_for_java_properties() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
reqFuel = scalar, U08, 0, "ms", 0.1, 0, 0, 25.5, 1
"#;

    let sandbox = TempDir::new("create-non-ascii");
    let request = CreateRequest {
        parent: sandbox.path().to_path_buf(),
        name: "Café".to_owned(),
        ini_path: sandbox.definition(DEFINITION),
        expected_signature: None,
        settings: Vec::new(),
        connection: None,
        layout: ProjectLayout::default(),
        write_stamp: None,
    };
    let project = Project::create(&request).expect("creation succeeds");

    let bytes =
        fs::read(project.root().join("projectCfg/project.properties")).expect("properties exist");
    assert!(
        bytes.is_ascii(),
        "TunerStudio reads ISO-8859-1, so the file stays ASCII: {bytes:?}"
    );
    let text = String::from_utf8(bytes).expect("ASCII is UTF-8");
    assert!(
        text.contains("projectName=Caf\\u00E9"),
        "an accented name is escaped, not encoded: {text}"
    );
}

#[test]
fn creating_a_project_refuses_a_folder_that_already_exists() {
    let sandbox = TempDir::new("create-existing-folder");
    let target = sandbox.path().join("Taken");
    fs::create_dir_all(&target).expect("the target exists");
    fs::write(target.join("existing.txt"), "x").expect("the target holds a file");

    let request = CreateRequest {
        parent: sandbox.path().to_path_buf(),
        name: "Taken".to_owned(),
        ini_path: sandbox.path().join("unused.ini"),
        expected_signature: None,
        settings: Vec::new(),
        connection: None,
        layout: ProjectLayout::default(),
        write_stamp: None,
    };
    assert!(matches!(
        Project::create(&request),
        Err(ProjectError::TargetExists { .. })
    ));
    assert!(target.join("existing.txt").is_file());
}

#[test]
fn creating_a_project_refuses_an_empty_folder_that_already_exists() {
    // A folder that looks empty in a picker may hold a hidden file (Finder
    // writes `.DS_Store` on open), so creation only writes into a folder it
    // made itself.
    let sandbox = TempDir::new("create-empty-folder");
    let target = sandbox.path().join("Empty");
    fs::create_dir_all(&target).expect("the target exists");

    let request = CreateRequest {
        parent: sandbox.path().to_path_buf(),
        name: "Empty".to_owned(),
        ini_path: sandbox.path().join("unused.ini"),
        expected_signature: None,
        settings: Vec::new(),
        connection: None,
        layout: ProjectLayout::default(),
        write_stamp: None,
    };
    assert!(matches!(
        Project::create(&request),
        Err(ProjectError::TargetExists { .. })
    ));
    assert!(target.is_dir());
    assert_eq!(
        fs::read_dir(&target).expect("readable").count(),
        0,
        "the existing folder is left untouched"
    );
}

#[test]
fn creating_a_project_refuses_a_file_that_holds_the_name() {
    let sandbox = TempDir::new("create-existing-file");
    let target = sandbox.path().join("Taken");
    fs::write(&target, "not a folder").expect("the file is writable");

    let request = CreateRequest {
        parent: sandbox.path().to_path_buf(),
        name: "Taken".to_owned(),
        ini_path: sandbox.path().join("unused.ini"),
        expected_signature: None,
        settings: Vec::new(),
        connection: None,
        layout: ProjectLayout::default(),
        write_stamp: None,
    };
    assert!(matches!(
        Project::create(&request),
        Err(ProjectError::TargetExists { .. })
    ));
    assert_eq!(
        fs::read_to_string(&target).expect("the file is readable"),
        "not a folder",
        "the file is left untouched"
    );
}

#[test]
fn creating_a_project_refuses_an_unusable_name() {
    let sandbox = TempDir::new("create-bad-name");
    let request = CreateRequest {
        parent: sandbox.path().to_path_buf(),
        name: "../escape".to_owned(),
        ini_path: sandbox.path().join("unused.ini"),
        expected_signature: None,
        settings: Vec::new(),
        connection: None,
        layout: ProjectLayout::default(),
        write_stamp: None,
    };
    assert!(matches!(
        Project::create(&request),
        Err(ProjectError::InvalidName { .. })
    ));
    assert!(sandbox.path().read_dir().unwrap().next().is_none());
}

#[test]
fn creating_a_project_checks_the_detected_signature() {
    let sandbox = TempDir::new("create-mismatch");
    let request = CreateRequest {
        parent: sandbox.path().to_path_buf(),
        name: "Mismatch".to_owned(),
        ini_path: sandbox.definition("[MegaTune]\nsignature = \"speeduino 202501\"\n"),
        expected_signature: Some("other 9999".to_owned()),
        settings: Vec::new(),
        connection: None,
        layout: ProjectLayout::default(),
        write_stamp: None,
    };
    assert!(matches!(
        Project::create(&request),
        Err(ProjectError::SignatureMismatch { .. })
    ));
    assert!(
        !sandbox.path().join("Mismatch").exists(),
        "a failed create leaves nothing behind"
    );
}

#[test]
fn creating_a_project_refuses_a_definition_that_is_not_ini() {
    let sandbox = TempDir::new("create-not-ini");
    sandbox.write("notes.txt", "not a definition");
    let request = CreateRequest {
        parent: sandbox.path().to_path_buf(),
        name: "Notes".to_owned(),
        ini_path: sandbox.path().join("notes.txt"),
        expected_signature: None,
        settings: Vec::new(),
        connection: None,
        layout: ProjectLayout::default(),
        write_stamp: None,
    };

    assert!(matches!(
        Project::create(&request),
        Err(ProjectError::DefinitionNotIni { .. })
    ));
    assert!(!sandbox.path().join("Notes").exists());
}

#[test]
fn creating_a_project_refuses_an_unreadable_definition() {
    let sandbox = TempDir::new("create-bad-ini");
    let request = CreateRequest {
        parent: sandbox.path().to_path_buf(),
        name: "Broken".to_owned(),
        ini_path: sandbox.definition("this is not an ini file\n"),
        expected_signature: None,
        settings: Vec::new(),
        connection: None,
        layout: ProjectLayout::default(),
        write_stamp: None,
    };
    assert!(matches!(
        Project::create(&request),
        Err(ProjectError::Ini(_))
    ));
    assert!(
        !sandbox.path().join("Broken").exists(),
        "a failed create leaves nothing behind"
    );
}

#[test]
fn a_newer_sidecar_is_left_untouched() {
    let dir = project_dir("newer-state");
    let before = r#"{"schema_version": 999, "connection": {"last_port": "/dev/ttyACM0"}}"#;
    dir.write(".oxtuner/project.json", before);

    let mut project = Project::open(dir.path()).expect("the project still opens");
    assert!(project.state().connection.last_port.is_none());

    project.save().expect("the save succeeds");
    project
        .set_connection(Some("/dev/ttyUSB0".to_owned()), None)
        .expect("the device updates in memory");
    assert_eq!(
        project.state().connection.last_port.as_deref(),
        Some("/dev/ttyUSB0")
    );

    let after = fs::read(dir.path().join(".oxtuner/project.json")).expect("the file is kept");
    assert_eq!(
        after,
        before.as_bytes(),
        "a newer sidecar is never overwritten"
    );
}

#[test]
fn a_standalone_tune_ignores_a_neighboring_sidecar() {
    let dir = TempDir::new("standalone-neighbor");
    dir.write("project/tune.msq", STANDALONE_TUNE);
    dir.write("project/standalone.ini", STANDALONE_DEFINITION);
    dir.write(
        "project/.oxtuner/project.json",
        r#"{"schema_version": 1, "connection": {"last_port": "/dev/ttyACM0"}}"#,
    );

    let project = Project::open_standalone(
        dir.path().join("project/tune.msq"),
        dir.path().join("project/standalone.ini"),
    )
    .expect("the tune opens");
    assert!(project.state().connection.last_port.is_none());
}

#[test]
fn creating_a_project_refuses_a_definition_with_includes() {
    const INCLUDING: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
#include page2.ini
"#;

    let sandbox = TempDir::new("create-includes");
    let request = CreateRequest {
        parent: sandbox.path().to_path_buf(),
        name: "Includes".to_owned(),
        ini_path: sandbox.definition(INCLUDING),
        expected_signature: None,
        settings: Vec::new(),
        connection: None,
        layout: ProjectLayout::default(),
        write_stamp: None,
    };
    let error = Project::create(&request).expect_err("an include cannot be copied");
    assert!(
        matches!(&error, ProjectError::DefinitionNeedsIncludes { names } if names == &["page2.ini"]),
        "{error}"
    );
    assert!(!sandbox.path().join("Includes").exists());
}

#[test]
fn the_layout_section_is_per_project_and_survives_a_reopen() {
    let dir = project_dir("ui-state");
    let mut project = Project::open(dir.path()).expect("the project opens");
    assert!(project.state().layout.primary_sidebar_visible);

    let layout = ProjectLayout {
        primary_sidebar_width: 300.0,
        secondary_sidebar_width: 200.0,
        bottom_panel_height: 260.0,
        primary_sidebar_visible: false,
        secondary_sidebar_visible: true,
        bottom_panel_visible: false,
        table_low_at_top: true,
    };
    project
        .set_layout(layout.clone())
        .expect("the layout persists");
    drop(project);

    let reopened = Project::open(dir.path()).expect("the project reopens");
    assert_eq!(reopened.state().layout, layout);

    let raw = std::fs::read_to_string(dir.path().join(".oxtuner/project.json"))
        .expect("the state file is readable");
    assert!(
        raw.contains("\"layout\""),
        "new writes use the layout key, not the old ui one"
    );
}

/// Creating a project from the real Speeduino definition runs only on the
/// owner's machine.
///
/// Set `OXTUNER_REFERENCE_DEFINITION` to the definition file and run
/// `cargo test -p oxtuner-project -- --ignored`. The definition is read, never
/// written, and the project goes to a temporary directory.
#[test]
#[ignore = "needs a local definition file (OXTUNER_REFERENCE_DEFINITION)"]
fn a_project_created_from_the_reference_definition_opens_clean() {
    let Ok(source) = std::env::var("OXTUNER_REFERENCE_DEFINITION") else {
        return;
    };
    let bytes = fs::read(source).expect("the reference definition is readable");

    let sandbox = TempDir::new("create-reference");
    let reference = sandbox.path().join("reference.ini");
    fs::write(&reference, &bytes).expect("the reference is copied to the sandbox");
    let request = CreateRequest {
        parent: sandbox.path().to_path_buf(),
        name: "Reference".to_owned(),
        ini_path: reference,
        expected_signature: None,
        settings: Vec::new(),
        connection: None,
        layout: ProjectLayout::default(),
        write_stamp: None,
    };
    let project = Project::create(&request).expect("creation succeeds");

    assert!(!project.is_read_only(), "{:?}", project.report().blockers);
    assert!(
        !project.definition().pages.is_empty(),
        "the definition declares at least one page"
    );
    assert!(
        project.report().missing_values.is_empty(),
        "a scaffold writes every variable the definition declares: {:?}",
        project.report().missing_values
    );
    assert!(
        project
            .report()
            .warnings
            .iter()
            .all(|issue| !issue.message.contains("presentation attribute")),
        "every presentation attribute must resolve on a scaffolded tune: {:?}",
        project.report().warnings
    );

    // A fresh definition has values below its declared minimums (the byte is
    // zero), so the open reports them as out of range and keeps them. The
    // byte-for-byte comparison below is the invariant that matters.
    let tune = fs::read(project.tune_path()).expect("the scaffolded tune is readable");
    drop(project);

    let root = sandbox.path().join("Reference");
    let mut reopened = Project::open(&root).expect("the created project reopens");
    assert!(!reopened.is_read_only(), "{:?}", reopened.report().blockers);
    assert_eq!(
        reopened
            .snapshot_msq()
            .expect("the working image serializes"),
        tune,
        "a created tune must read back as the file it was written from"
    );

    let outcome = reopened.save().expect("an unedited scaffold saves");
    assert_eq!(outcome.cells, 0);
    assert_eq!(
        fs::read(reopened.tune_path()).expect("the tune is readable"),
        tune,
        "opening and saving a scaffold must not rewrite it"
    );
}

#[test]
fn a_chosen_setting_group_compiles_the_branch_and_is_recorded() {
    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
reqFuel = scalar, U08, 0, "ms", 0.1, 0, 0, 25.5, 1

[SettingGroups]
settingGroup = pressure_units, "Pressure Display"
settingOption = DEFAULT, "PSI"
settingOption = pressure_bar, "BAR"
"#;

    let sandbox = TempDir::new("create-settings");
    let request = CreateRequest {
        parent: sandbox.path().to_path_buf(),
        name: "Bar".to_owned(),
        ini_path: sandbox.definition(DEFINITION),
        expected_signature: None,
        settings: vec!["pressure_bar".to_owned()],
        connection: None,
        layout: ProjectLayout::default(),
        write_stamp: None,
    };
    let project = Project::create(&request).expect("creation succeeds");

    assert!(
        project.settings().is_active("pressure_bar"),
        "the chosen option is active for the definition"
    );

    let tune = fs::read_to_string(project.tune_path()).expect("the tune is readable");
    assert!(
        tune.contains("pressure_bar"),
        "the snapshot records the choice: {tune}"
    );
    assert!(
        !tune.contains("DEFAULT"),
        "the default option is not a flag: {tune}"
    );

    let properties = fs::read_to_string(project.root().join("projectCfg/project.properties"))
        .expect("properties exist");
    assert!(
        properties.contains("ecuSettings=pressure_bar|"),
        "TunerStudio reads the choice from there: {properties}"
    );
}

#[test]
fn creation_remembers_the_connection_the_wizard_picked() {
    let sandbox = TempDir::new("create-connection");
    let request = CreateRequest {
        parent: sandbox.path().to_path_buf(),
        name: "Roadster".to_owned(),
        ini_path: sandbox.definition(CONNECTION_DEFINITION),
        expected_signature: None,
        settings: Vec::new(),
        connection: Some(ConnectionSeed {
            port: "/dev/cu.test".to_owned(),
            baud: 57_600,
        }),
        layout: ProjectLayout::default(),
        write_stamp: None,
    };
    Project::create(&request).expect("creation succeeds");

    let root = sandbox.path().join("Roadster");
    let reopened = Project::open(&root).expect("the project reopens");
    assert_eq!(
        reopened.state().connection.last_port.as_deref(),
        Some("/dev/cu.test")
    );
    assert_eq!(reopened.state().connection.baud_override, Some(57_600));
}

#[test]
fn creation_without_a_baud_keeps_the_definition_rate() {
    let sandbox = TempDir::new("create-no-baud");
    let request = CreateRequest {
        parent: sandbox.path().to_path_buf(),
        name: "Roadster".to_owned(),
        ini_path: sandbox.definition(CONNECTION_DEFINITION),
        expected_signature: None,
        settings: Vec::new(),
        connection: None,
        layout: ProjectLayout::default(),
        write_stamp: None,
    };
    let project = Project::create(&request).expect("creation succeeds");

    assert_eq!(project.state().connection.last_port, None);
    assert_eq!(project.state().connection.baud_override, None);
}

const ALIAS_DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 16
page = 1
data = array, U08, 0, [4], "", 1, 0, 0, 255, 0
data0 = bits, U08, 0, [0:7]
"#;

#[test]
fn a_view_that_carries_its_owners_bytes_survives_a_save() {
    const TUNE: &str = r#"<?xml version="1.0" encoding="ISO-8859-1"?>
<msq xmlns="http://www.msefi.com/:msq">
    <versionInfo fileFormat="5.0" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="16">
        <constant name="data0">5.0</constant>
    </page>
</msq>
"#;
    let dir = TempDir::new("alias-only-save");
    dir.write("projectCfg/mainController.ini", ALIAS_DEFINITION);
    dir.write(TUNE_FILE, TUNE);

    let mut project = Project::open(dir.path()).expect("the project opens");
    assert_eq!(project.tune().raw_at("data", 0).unwrap(), 5.0);

    project.save().expect("an unedited project saves");

    let reopened = Project::open(dir.path()).expect("the project reopens");
    assert_eq!(
        reopened.tune().raw_at("data", 0).unwrap(),
        5.0,
        "removing the view would lose the only entry that carried the byte"
    );
}

#[test]
fn a_saved_view_is_not_reported_missing_on_reopen() {
    const TUNE: &str = r#"<?xml version="1.0" encoding="ISO-8859-1"?>
<msq xmlns="http://www.msefi.com/:msq">
    <versionInfo fileFormat="5.0" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="16">
        <constant cols="1" name="data" rows="4">5.0 6.0 7.0 8.0</constant>
    </page>
</msq>
"#;
    let dir = TempDir::new("alias-save-warning");
    dir.write("projectCfg/mainController.ini", ALIAS_DEFINITION);
    dir.write(TUNE_FILE, TUNE);

    let mut project = Project::open(dir.path()).expect("the project opens");
    project.save().expect("the project saves");
    let reopened = Project::open(dir.path()).expect("the project reopens");

    let missing: Vec<&str> = reopened
        .report()
        .warnings
        .iter()
        .filter(|issue| issue.kind == IssueKind::MissingValue)
        .map(|issue| issue.message.as_str())
        .collect();
    assert!(missing.is_empty(), "{missing:?}");
    assert_eq!(reopened.tune().scalar_raw("data0").unwrap(), 5.0);
}

#[test]
fn a_pc_edit_lands_where_the_project_reads_it() {
    let dir = project_dir("pc-save");
    let mut project = Project::open(dir.path()).expect("the project opens");
    project
        .tune_mut()
        .set_user("rpmhigh", 10_000.0)
        .expect("the PC value is editable");

    project.save().expect("the project saves");

    assert!(
        dir.path().join("projectCfg/pcVariableValues.msq").exists(),
        "a PC edit is written to the sidecar the project reads"
    );
    let reopened = Project::open(dir.path()).expect("the project reopens");
    assert_eq!(reopened.tune().scalar_user("rpmhigh").unwrap(), 10_000.0);
}
