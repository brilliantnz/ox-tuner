use std::sync::Arc;

use crate::fields::CommandTemplate;

use super::*;

#[test]
fn the_structural_hash_tracks_renames_but_not_comments() {
    let base = "\
[MegaTune]
signature = \"hash test\"
[Constants]
nPages = 1
pageSize = 64
page = 1
first = scalar, U08, 0, \"u\", 1, 0, 0, 255, 0
";
    let renamed = base.replace("first =", "second =");
    let commented = format!("{base}; a comment that changes no model data\n");

    let parse =
        |text: &str| crate::parse_str(text, &crate::SettingsContext::new()).expect("parses");
    let original = parse(base).structural_hash();

    assert_eq!(parse(&commented).structural_hash(), original);
    assert_ne!(parse(&renamed).structural_hash(), original);
}

#[test]
fn an_ordered_collection_keeps_definition_order_and_replaces_in_place() {
    let mut constants: Ordered<u32> = Ordered::new();
    constants.insert("first", 1);
    constants.insert("second", 2);
    constants.insert("first", 10);

    assert_eq!(
        constants.names().collect::<Vec<_>>(),
        vec!["first", "second"]
    );
    assert_eq!(constants.get("first"), Some(&10));
    assert_eq!(constants.len(), 2);
}

#[test]
fn a_lookup_falls_back_to_case_insensitive_matching() {
    let mut constants: Ordered<u32> = Ordered::new();
    constants.insert("reqFuel", 1);

    assert_eq!(constants.get_ignore_case("REQFUEL"), Some(&1));
    assert_eq!(constants.get("REQFUEL"), None);
}

#[test]
fn a_case_insensitive_lookup_returns_the_first_declaration() {
    let mut constants: Ordered<u32> = Ordered::new();
    constants.insert("reqFuel", 1);
    constants.insert("reqfuel", 2);
    constants.insert("REQFUEL", 3);

    assert_eq!(constants.get_ignore_case("ReqFuel"), Some(&1));
    assert_eq!(constants.get("reqfuel"), Some(&2));
    assert_eq!(
        constants.names().collect::<Vec<_>>(),
        vec!["reqFuel", "reqfuel", "REQFUEL"],
        "the index must not reorder or merge entries"
    );
}

#[test]
fn a_case_insensitive_lookup_trims_the_query_but_not_the_entry() {
    let mut constants: Ordered<u32> = Ordered::new();
    constants.insert(" spaced ", 1);

    assert_eq!(
        constants.get_ignore_case("spaced"),
        None,
        "a stored name keeps its spaces, matching the linear scan this replaced"
    );
    assert_eq!(constants.get_ignore_case("  spaced  "), None);
}

#[test]
fn redefining_a_name_keeps_order_and_the_index_consistent() {
    let mut constants: Ordered<u32> = Ordered::new();
    constants.insert("first", 1);
    constants.insert("second", 2);
    constants.insert("First", 3);
    constants.insert("first", 10);

    assert_eq!(constants.get_ignore_case("FIRST"), Some(&10));
    assert_eq!(constants.get("First"), Some(&3));
    assert_eq!(
        constants.names().collect::<Vec<_>>(),
        vec!["first", "second", "First"]
    );
    assert_eq!(constants.len(), 3);
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(256))]

    /// The map-backed lookup must agree with the linear scan it replaced.
    #[test]
    fn a_case_insensitive_lookup_matches_a_linear_scan(
        entries in proptest::collection::vec(
            ("[A-Za-z0-9_ ]{0,10}", proptest::prelude::any::<u32>()),
            0..40,
        ),
        queries in proptest::collection::vec("[A-Za-z0-9_ ]{0,12}", 0..24),
    ) {
        let mut ordered: Ordered<u32> = Ordered::new();
        for (name, value) in &entries {
            ordered.insert(name.clone(), *value);
        }

        for query in queries {
            let expected = ordered
                .get(&query)
                .or_else(|| {
                    ordered
                        .entries
                        .iter()
                        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(query.trim_ascii()))
                        .map(|(_, value)| value)
                })
                .copied();

            proptest::prop_assert_eq!(ordered.get_ignore_case(&query).copied(), expected);
        }
    }
}

#[test]
fn a_table_shape_is_column_first() {
    // rusEFI's `[6x2]` is six columns by two rows: the first token is the
    // column count.
    let shape = Shape::Table {
        cols: Dimension::Fixed(6),
        rows: Dimension::Fixed(2),
    };

    assert_eq!(shape.element_count(), Some(12));
}

#[test]
fn a_table_shape_whose_product_overflows_u32_has_no_element_count() {
    // 65 536 columns by 65 536 rows is 2^32 elements.
    let shape = Shape::Table {
        cols: Dimension::Fixed(65_536),
        rows: Dimension::Fixed(65_536),
    };

    assert_eq!(shape.element_count(), None);
    assert!(!shape.is_dynamic());
}

#[test]
fn a_table_shape_just_under_the_overflow_boundary_still_has_an_element_count() {
    let shape = Shape::Table {
        cols: Dimension::Fixed(65_535),
        rows: Dimension::Fixed(65_535),
    };

    assert_eq!(shape.element_count(), Some(4_294_836_225));
}

#[test]
fn a_bit_shape_reports_its_option_count() {
    let shape = Shape::Bits {
        low: 0,
        high: 2,
        shift: 0,
    };

    assert_eq!(shape.element_count(), Some(8));
}

#[test]
fn a_data_type_reports_its_width() {
    assert_eq!(DataType::parse("U08").map(DataType::size), Some(1));
    assert_eq!(DataType::parse("s16").map(DataType::size), Some(2));
    assert_eq!(DataType::parse("F32").map(DataType::size), Some(4));
    assert_eq!(DataType::parse("nonsense"), None);
}

#[test]
fn a_constant_reports_its_size_only_when_the_shape_is_known() {
    let constant = Constant {
        name: "veTable".to_owned(),
        class: ConstantClass::Array,
        data_type: DataType::U16,
        page: 2,
        offset: Offset::Fixed(196),
        address: Some(196),
        shape: Shape::Table {
            cols: Dimension::Fixed(16),
            rows: Dimension::Fixed(16),
        },
        units: Text::shared("%"),
        scale: Some(Number::Literal(0.1)),
        translate: None,
        minimum: None,
        maximum: None,
        digits: None,
        options: Arc::from([]),
        flags: ConstantFlags::default(),
        line: 1,
        raw: String::new(),
    };

    assert_eq!(constant.byte_size(), Some(512));
}

#[test]
fn a_page_is_burnable_only_with_a_non_empty_command() {
    let mut page = Page::default();
    assert!(!page.is_burnable());

    page.burn_command = Some(CommandTemplate::parse(r#""""#));
    assert!(!page.is_burnable());

    page.burn_command = Some(CommandTemplate::parse(r#""B%2i""#));
    assert!(page.is_burnable());
}

#[test]
fn a_cached_definition_must_look_like_the_parser_could_have_produced_it() {
    assert!(!Definition::empty().is_usable());

    let mut definition = Definition::empty();
    definition.signature = "speeduino 202501".to_owned();
    assert!(definition.is_usable());

    definition.spec_version = Some(crate::MAX_SPEC_VERSION + 1.0);
    assert!(
        !definition.is_usable(),
        "a spec this build cannot read is not a usable cache entry"
    );
}
