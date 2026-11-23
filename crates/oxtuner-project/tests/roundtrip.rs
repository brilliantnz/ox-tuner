//! Property tests for the file boundary: raw values must survive a write/read
//! round-trip, and hostile bytes must never panic the reader.

#![expect(
    clippy::unwrap_used,
    clippy::float_cmp,
    reason = "the property test unwraps fixture setup and compares raw values exactly"
)]

use std::sync::{Arc, OnceLock};

use oxtuner_ini::{Definition, SettingsContext, parse_str};
use oxtuner_project::{
    CreateRequest, Msq, Project, ProjectLayout, WritePolicy, format_values, load,
};
use oxtuner_tune::{EntryShape, IniPage, Tune, TuneLayout, Values, VariableSpec, decode_page};
use proptest::prelude::*;

const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 64
page = 1
reqFuel = scalar, U08, 0, "ms", 0.1, 0, 0, 255, 1
table = array, U08, 4, [2x2], "", 1, 0, 0, 255, 0
label = string, ASCII, 8
"#;

fn definition() -> &'static Definition {
    static PARSED: OnceLock<Definition> = OnceLock::new();
    PARSED.get_or_init(|| {
        parse_str(DEFINITION, &SettingsContext::new()).expect("the fixture definition parses")
    })
}

fn layout() -> &'static TuneLayout {
    static BUILT: OnceLock<TuneLayout> = OnceLock::new();
    BUILT.get_or_init(|| TuneLayout::build(definition(), &|_| None))
}

fn tune_with(entries: &str) -> String {
    format!(
        r#"<msq>
    <versionInfo fileFormat="5.0" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="64">{entries}</page>
</msq>"#
    )
}

proptest! {
    #[test]
    fn a_scalar_value_survives_file_text(raw in 0u16..=255) {
        let spec = layout().variable("reqFuel").expect("reqFuel is in the definition");
        let variables = Values::Numbers(vec![f64::from(raw)]);
        let text = format_values(spec, definition(), &variables).expect("a scalar formats");
        let source = tune_with(&format!("<constant name=\"reqFuel\">{text}</constant>"));
        let msq = Msq::parse(source.as_bytes()).expect("the generated tune parses");
        let loaded = load(&msq, definition());

        prop_assert!(loaded.report.blockers.is_empty(), "{:?}", loaded.report.blockers);
        prop_assert_eq!(loaded.tune.scalar_raw("reqFuel").unwrap(), f64::from(raw));
    }

    #[test]
    fn a_table_value_survives_file_text(raw in proptest::array::uniform4(0u8..=255)) {
        let spec = layout().variable("table").expect("table is in the definition");
        let values = raw.iter().copied().map(f64::from).collect();
        let text = format_values(spec, definition(), &Values::Numbers(values)).expect("a table formats");
        let source = tune_with(&format!(
            "<constant cols=\"2\" rows=\"2\" name=\"table\">{text}</constant>"
        ));
        let msq = Msq::parse(source.as_bytes()).expect("the generated tune parses");
        let loaded = load(&msq, definition());

        prop_assert!(loaded.report.blockers.is_empty(), "{:?}", loaded.report.blockers);
        for (index, expected) in raw.iter().enumerate() {
            prop_assert_eq!(
                loaded.tune.raw_at("table", index).unwrap(),
                f64::from(*expected)
            );
        }
    }

    #[test]
    fn a_text_value_survives_file_text(text in "[A-Za-z0-9]{0,8}") {
        let spec = layout().variable("label").expect("label is in the definition");
        let formatted = format_values(spec, definition(), &Values::Text(text.clone())).expect("ASCII text formats");
        let source = tune_with(&format!("<constant name=\"label\">{formatted}</constant>"));
        let msq = Msq::parse(source.as_bytes()).expect("the generated tune parses");
        let loaded = load(&msq, definition());

        prop_assert!(loaded.report.blockers.is_empty(), "{:?}", loaded.report.blockers);
        prop_assert_eq!(loaded.tune.text("label").unwrap(), text.as_str());
    }

    #[test]
    fn arbitrary_bytes_never_panic_the_reader(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let _ = Msq::parse(&bytes);
    }

    #[test]
    fn arbitrary_text_never_panics_the_reader(text in "\\PC{0,512}") {
        let _ = Msq::parse(text.as_bytes());
    }
}

/// A definition whose every declared page byte has a text form: each bit field
/// lists an option for every value its bits can hold, every scale is a constant,
/// and `narrow` declares a range tighter than its storage so the range path is
/// covered too. `hiddenGap` repeats a hidden label away from zero, the shape a
/// real definition uses for the values it does not offer. `invalidFirst`
/// declares a non-zero default, so leaving its INVALID value out is visible on
/// the next open. Bytes no constant covers are padding and are not compared.
const IMAGE_DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 2
pageSize = 16, 8
page = 1
scalarA = scalar, U08, 0, "ms", 0.1, 0, 0, 25.5, 1
bitsA = bits, U08, 1, [0:1], "Off", "One", "Two", "Three"
invalidFirst = bits, U08, 2, [0:2], "INVALID", "One", "Two", "Three", "Four", "Five", "Six", "Seven"
narrow = scalar, U08, 3, "", 1, 0, 0, 200, 0
peerA = array, U08, 4, [2x2], "AFR", 0.1, 0, 0, 25.5, 1
peerB = array, U08, 4, [2x2], "Lambda", 1, 0, 0, 255, 0
wide = scalar, U16, 8, "", 1, 0, 0, 65535, 0
hiddenGap = bits, U08, 10, [0:2], "INVALID", "One", "INVALID", "Three"
page = 2
second = scalar, S08, 0, "", 1, 0, -128, 127, 0

[Defaults]
defaultValue = invalidFirst, 5
"#;

/// A tune whose page values are `page1` and `page2`, decoded the way a read
/// from the controller decodes them.
fn tune_from_pages(definition: &Definition, page1: &[u8], page2: &[u8]) -> Tune {
    let mut tune = Tune::from_definition(definition);
    let layout = Arc::clone(tune.layout());

    for (number, bytes) in [(1u8, page1), (2u8, page2)] {
        let page = IniPage::new(number).expect("the fixture declares the page");
        let mut decoded = tune.snapshot();
        decode_page(&layout, page, bytes, &mut decoded).expect("the page decodes");
        tune.adopt_values(&decoded)
            .expect("the decoded values are adopted");
    }

    tune
}

/// Whether `raw` is one of the options the definition hides as `"INVALID"`.
///
/// Mirrors the crate-private `msq::selects_hidden_option`. It uses the same
/// `any` spelling, not the first match: an option value is unique in practice,
/// but the test oracle must not take a different branch if it is not.
fn hidden_value(definition: &Definition, spec: &VariableSpec, raw: f64) -> bool {
    if !matches!(spec.shape, EntryShape::Bits { .. }) {
        return false;
    }

    definition.constant(&spec.name).is_some_and(|constant| {
        constant
            .options
            .iter()
            .any(|option| option.hidden && f64::from(option.value) == raw)
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        ..ProptestConfig::default()
    })]

    /// A project created from a controller image carries every tuning value.
    ///
    /// The definition is built so every declared page byte has a text form, the
    /// way a vendored definition does for the values it declares. Whatever the
    /// controller reported, `create(Full)` followed by an open must read back
    /// the same raw values, including a value above a range narrower than its
    /// storage. A value the definition offers only as the hidden `"INVALID"`
    /// label is not a tuning value: the reference writer leaves the entry out
    /// and the definition's default applies on open.
    #[test]
    fn a_project_created_from_arbitrary_page_bytes_keeps_every_tuning_value(
        page1 in proptest::collection::vec(any::<u8>(), 16),
        page2 in proptest::collection::vec(any::<u8>(), 8),
    ) {
        let definition =
            parse_str(IMAGE_DEFINITION, &SettingsContext::new()).expect("the fixture parses");
        let seed = tune_from_pages(&definition, &page1, &page2);

        let sandbox = tempfile::TempDir::new().expect("a temporary directory");
        let ini_path = sandbox.path().join("mainController.ini");
        std::fs::write(&ini_path, IMAGE_DEFINITION).expect("the fixture definition is written");
        let request = CreateRequest {
            parent: sandbox.path().to_path_buf(),
            name: "Image".to_owned(),
            ini_path,
            expected_signature: None,
            settings: Vec::new(),
            connection: None,
            layout: ProjectLayout::default(),
            write_stamp: None,
        };
        let project = Project::prepare(&request)
            .expect("the request prepares")
            .create(Some(&seed), WritePolicy::Full, None)
            .expect("the project is created");
        let root = project.root().to_path_buf();
        drop(project);

        let reopened = Project::open(&root).expect("the created project opens");
        prop_assert!(!reopened.is_read_only(), "{:?}", reopened.report().blockers);
        let written = std::fs::read_to_string(root.join("CurrentTune.msq"))
            .expect("the created tune reads");

        for spec in seed.layout().variables() {
            if spec.ini_page().is_none() || !spec.shape.is_numeric() {
                continue;
            }
            for index in 0..spec.element_count() {
                let seeded = seed.raw_at(&spec.name, index).expect("the seed holds it");
                let opened = reopened
                    .tune()
                    .raw_at(&spec.name, index)
                    .expect("the project holds it");
                if hidden_value(&definition, spec, seeded) {
                    prop_assert!(
                        !written.contains(&format!("name=\"{}\"", spec.name)),
                        "`{}` is INVALID and must stay out of the tune",
                        spec.name
                    );
                    prop_assert_eq!(
                        opened, spec.default_raw,
                        "`{}` is INVALID and must open at the default",
                        spec.name
                    );
                } else {
                    prop_assert_eq!(
                        seeded, opened,
                        "`{}`[{}] changed across create and open",
                        spec.name, index
                    );
                }
            }
        }
    }
}

#[test]
fn a_seeded_scaffold_omits_both_hidden_shapes() {
    // The property test almost certainly sees a hidden value, but it cannot
    // assert that it does. Pin both shapes deterministically: the option-0
    // placeholder and a repeated hidden label away from zero.
    let definition =
        parse_str(IMAGE_DEFINITION, &SettingsContext::new()).expect("the fixture parses");
    let mut page1 = vec![0u8; 16];
    page1[2] = 0; // `invalidFirst` selects the hidden option 0.
    page1[10] = 0b10; // `hiddenGap` selects the hidden option 2.
    let seed = tune_from_pages(&definition, &page1, &[0u8; 8]);

    let sandbox = tempfile::TempDir::new().expect("a temporary directory");
    let ini_path = sandbox.path().join("mainController.ini");
    std::fs::write(&ini_path, IMAGE_DEFINITION).expect("the fixture definition is written");
    let request = CreateRequest {
        parent: sandbox.path().to_path_buf(),
        name: "Hidden".to_owned(),
        ini_path,
        expected_signature: None,
        settings: Vec::new(),
        connection: None,
        layout: ProjectLayout::default(),
        write_stamp: None,
    };
    let project = Project::prepare(&request)
        .expect("the request prepares")
        .create(Some(&seed), WritePolicy::Full, None)
        .expect("the project is created");
    let root = project.root().to_path_buf();
    drop(project);

    let written = std::fs::read_to_string(root.join("CurrentTune.msq")).expect("the tune reads");
    for name in ["invalidFirst", "hiddenGap"] {
        assert!(
            !written.contains(&format!("name=\"{name}\"")),
            "`{name}` is INVALID and must stay out of the tune: {written}"
        );
    }
    assert!(
        written.contains("name=\"bitsA\""),
        "a representable value is still written: {written}"
    );

    let reopened = Project::open(&root).expect("the created project opens");
    assert_eq!(
        reopened.tune().raw_at("bitsA", 0).expect("it opens"),
        0.0,
        "a representable value round-trips, not just writes"
    );
    assert_eq!(
        reopened.tune().raw_at("invalidFirst", 0).expect("it opens"),
        5.0,
        "the definition's non-zero default applies"
    );
    assert_eq!(
        reopened.tune().raw_at("hiddenGap", 0).expect("it opens"),
        0.0,
        "an undeclared default is raw zero"
    );
}

#[test]
fn a_text_value_outside_iso_8859_1_names_the_variable() {
    let spec = layout()
        .variable("label")
        .expect("label is in the definition");

    let error = format_values(spec, definition(), &Values::Text("→".to_owned()))
        .expect_err("the arrow has no ISO-8859-1 byte");

    assert!(error.to_string().contains("`label`"), "{error}");
    assert!(error.to_string().contains("ISO-8859-1"), "{error}");
}

#[test]
fn a_bit_label_outside_iso_8859_1_names_the_variable() {
    let definition = parse_str(
        "[MegaTune]\nsignature = \"x\"\n[Constants]\nnPages = 1\npageSize = 8\npage = 1\nmode = bits, U08, 0, [0:0], \"→\"\n",
        &SettingsContext::new(),
    )
    .expect("the definition parses");
    let layout = TuneLayout::build(&definition, &|_| None);
    let spec = layout.variable("mode").expect("mode is in the definition");

    let error = format_values(spec, &definition, &Values::Numbers(vec![0.0]))
        .expect_err("the label has no ISO-8859-1 byte");

    assert!(error.to_string().contains("`mode`"), "{error}");
    assert!(error.to_string().contains("ISO-8859-1"), "{error}");
}

#[test]
fn a_deeply_nested_document_is_rejected_without_overflowing_the_stack() {
    let deep = format!("{}{}", "<a>".repeat(1_000), "</a>".repeat(1_000));
    assert!(Msq::parse(format!("<msq>{deep}</msq>").as_bytes()).is_err());
}
