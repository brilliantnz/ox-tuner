//! The definition cache as a project sees it: hits, misses and corruption.
//!
//! The cache is always in a directory outside the project, which is what makes
//! a read-only project cacheable. A test that deletes the cache must be able
//! to open the project again with exactly the same definition.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use oxtuner_project::{DefinitionSource, Project, TUNE_FILE};

const DEFINITION: &str = "\
[MegaTune]
signature = \"cache 1\"
queryCommand = \"Q\"

#define benchLabels = \"Off\", \"On\"

[Constants]
nPages = 1
pageSize = 64
page = 1
reqFuel = scalar, U08, 0, \"ms\", 1, 0, 0, 255, 0
mode = bits, U08, 2, [0:1], \"Off\", \"On\"
unknownKey = 1
#if CELSIUS
tempUnits = scalar, U08, 1, \"C\", 1, 0, 0, 255, 0
#else
tempUnits = scalar, U08, 1, \"F\", 1, 0, 0, 255, 0
#endif

[Menu]
menu = \"Tuning\"
std_separator
";

const TUNE: &str = r#"<msq>
    <versionInfo fileFormat="5.0" nPages="1" signature="cache 1"/>
    <page number="0" size="64">
        <constant name="reqFuel">10.0</constant>
        <constant name="tempUnits">1.0</constant>
    </page>
</msq>
"#;

struct Fixture {
    base: PathBuf,
    root: PathBuf,
    cache: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "oxtuner-cache-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = base.join("project");
        let config = root.join("projectCfg");
        fs::create_dir_all(&config).unwrap();
        fs::write(config.join("mainController.ini"), DEFINITION).unwrap();
        fs::write(root.join(TUNE_FILE), TUNE).unwrap();

        // Deliberately outside the project: that is what makes a read-only
        // project cacheable in the application.
        let cache = base.join("cache");
        Self { base, root, cache }
    }

    fn open(&self) -> Project {
        Project::open_with_cache(&self.root, Some(&self.cache)).expect("the project opens")
    }

    fn definition(&self) -> PathBuf {
        self.root.join("projectCfg").join("mainController.ini")
    }

    fn tune(&self) -> PathBuf {
        self.root.join(TUNE_FILE)
    }

    fn cache_files(&self) -> Vec<PathBuf> {
        fs::read_dir(&self.cache)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.extension().is_some_and(|ext| ext == "defcache"))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn with_tune_setting(path: &Path, setting: &str) {
    let tune = TUNE.replace(
        "    </page>\n</msq>",
        &format!("    </page>\n    <settings>\n        {setting}\n    </settings>\n</msq>"),
    );
    fs::write(path, tune).unwrap();
}

#[test]
fn a_second_open_uses_the_cache_and_matches_a_fresh_parse() {
    let fixture = Fixture::new("hit");

    let first = fixture.open();
    assert_eq!(first.definition_source(), DefinitionSource::Parsed);
    let fresh = first.definition().clone();
    assert_eq!(
        fresh.defines.get("benchlabels").map(Vec::len),
        Some(2),
        "fixture must record the define"
    );
    assert!(
        !fresh.diagnostics.is_empty(),
        "fixture must produce a diagnostic for the cache to round-trip"
    );
    assert_eq!(fixture.cache_files().len(), 1, "a miss writes one entry");

    let second = fixture.open();
    assert_eq!(second.definition_source(), DefinitionSource::Cache);
    assert_eq!(
        second.definition(),
        &fresh,
        "a cache hit must be the definition a parse would have produced"
    );
    assert_eq!(second.tune().scalar_user("reqFuel").ok(), Some(10.0));
    assert_eq!(
        second
            .definition()
            .constant("mode")
            .expect("mode")
            .options
            .len(),
        2,
        "shared bit-option tables survive the round trip"
    );
    assert_eq!(
        second.definition().defines.get("benchlabels").map(Vec::len),
        Some(2),
        "the #define table survives the round trip"
    );
    assert_eq!(second.definition().menus.len(), 1);
}

#[test]
fn changing_the_definition_bytes_is_a_miss() {
    let fixture = Fixture::new("definition-change");

    fixture.open();
    let changed = DEFINITION.replace("cache 1", "cache 2");
    fs::write(fixture.definition(), changed).unwrap();

    let project = fixture.open();
    assert_eq!(project.definition_source(), DefinitionSource::Parsed);
    assert_eq!(project.definition().signature, "cache 2");
    assert_eq!(
        fixture.cache_files().len(),
        2,
        "a changed fingerprint gets its own entry"
    );
}

#[test]
fn changing_a_parser_setting_is_a_miss() {
    let fixture = Fixture::new("settings-change");

    let before = fixture.open();
    assert_eq!(before.definition_source(), DefinitionSource::Parsed);
    assert!(!before.settings().is_active("CELSIUS"));
    assert_eq!(
        before
            .definition()
            .constant("tempUnits")
            .unwrap()
            .units
            .to_string(),
        "F"
    );

    with_tune_setting(
        &fixture.tune(),
        r#"<setting name="CELSIUS" value="CELSIUS"/>"#,
    );

    let after = fixture.open();
    assert_eq!(after.definition_source(), DefinitionSource::Parsed);
    assert!(after.settings().is_active("CELSIUS"));
    assert_eq!(
        after
            .definition()
            .constant("tempUnits")
            .unwrap()
            .units
            .to_string(),
        "C"
    );

    let hit = fixture.open();
    assert_eq!(hit.definition_source(), DefinitionSource::Cache);
    assert_eq!(
        hit.definition()
            .constant("tempUnits")
            .unwrap()
            .units
            .to_string(),
        "C"
    );
}

#[test]
fn changing_an_included_file_is_a_miss() {
    let fixture = Fixture::new("include");
    let definition = DEFINITION.replace(
        "reqFuel = scalar, U08, 0, \"ms\", 1, 0, 0, 255, 0",
        "reqFuel = scalar, U08, 0, \"ms\", 1, 0, 0, 255, 0\n#include extra.inc",
    );
    fs::write(fixture.definition(), definition).unwrap();
    let config = fixture.root.join("projectCfg");
    fs::write(
        config.join("extra.inc"),
        "included = scalar, U08, 2, \"first\", 1, 0, 0, 255, 0\n",
    )
    .unwrap();

    let first = fixture.open();
    assert_eq!(first.definition_source(), DefinitionSource::Parsed);
    assert_eq!(
        first
            .definition()
            .constant("included")
            .unwrap()
            .units
            .to_string(),
        "first"
    );

    fs::write(
        config.join("extra.inc"),
        "included = scalar, U08, 2, \"second\", 1, 0, 0, 255, 0\n",
    )
    .unwrap();
    let second = fixture.open();
    assert_eq!(second.definition_source(), DefinitionSource::Parsed);
    assert_eq!(
        second
            .definition()
            .constant("included")
            .unwrap()
            .units
            .to_string(),
        "second"
    );
}

#[test]
fn a_corrupt_or_missing_cache_is_rebuilt() {
    let fixture = Fixture::new("corrupt");
    fixture.open();

    let entry = fixture.cache_files().remove(0);
    fs::write(&entry, b"not a cache").unwrap();
    assert_eq!(fixture.open().definition_source(), DefinitionSource::Parsed);

    fs::remove_dir_all(&fixture.cache).unwrap();
    let rebuilt = fixture.open();
    assert_eq!(rebuilt.definition_source(), DefinitionSource::Parsed);
    assert_eq!(fixture.cache_files().len(), 1);
}

#[test]
fn an_unwritable_cache_does_not_stop_the_project_from_opening() {
    let fixture = Fixture::new("unwritable");
    // A file where the cache directory should be: `create_dir_all` fails.
    fs::write(&fixture.cache, b"not a directory").unwrap();

    let project = fixture.open();
    assert_eq!(project.definition_source(), DefinitionSource::Parsed);
    assert_eq!(project.definition().signature, "cache 1");
}

#[cfg(unix)]
#[test]
fn a_read_only_project_still_uses_the_cache() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new("read-only");
    let original = fs::metadata(&fixture.root).unwrap().permissions();
    fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o555)).unwrap();

    let first = fixture.open();
    assert_eq!(first.definition_source(), DefinitionSource::Parsed);

    let second = fixture.open();
    assert_eq!(second.definition_source(), DefinitionSource::Cache);

    fs::set_permissions(&fixture.root, original).unwrap();
}
