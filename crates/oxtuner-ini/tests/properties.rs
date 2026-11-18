//! Property tests: the parser must be total.
//!
//! The contract is narrow and absolute — **any bytes produce either a
//! definition or a structured error, never a panic, never a hang**. Fuzzing
//! covers the same ground with coverage guidance; these tests run everywhere,
//! including the three-OS CI matrix, so a regression cannot depend on someone
//! remembering to run `cargo fuzz`.

use proptest::prelude::*;

use oxtuner_ini::{
    Dimension, IniError, SettingsContext, Shape, parse_bytes, parse_str, preprocess,
};

fn parse(text: &str) -> Result<oxtuner_ini::Definition, IniError> {
    parse_str(text, &SettingsContext::new())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    /// Arbitrary text never panics.
    #[test]
    fn arbitrary_text_never_panics(text in "\\PC{0,400}") {
        let _ = parse(&text);
    }

    /// Arbitrary bytes never panic, including invalid UTF-8 and BOMs.
    #[test]
    fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..400)) {
        let _ = parse_bytes(&bytes, &SettingsContext::new());
    }

    /// Text built from the dialect's own punctuation is the interesting case:
    /// quotes, braces, commas, `#` and `;` are where the tokenizer can trip.
    #[test]
    fn dialect_punctuation_never_panics(text in "[\\[\\]{}#;=,\"$%\\\\ \t\r\nA-Za-z0-9_+*/-]{0,300}") {
        let _ = parse(&text);
    }

    /// Preprocessing always terminates and either succeeds or returns an error.
    #[test]
    fn preprocessing_always_terminates(text in "\\PC{0,600}") {
        let _ = preprocess(&text, &SettingsContext::new(), None);
    }

    /// A table whose two dimensions are 65 536 or more always overflows the
    /// element count, and reports it as unknown instead of panicking or
    /// wrapping.
    #[test]
    fn a_table_element_count_never_wraps(
        cols in 65_536u32..=u32::MAX,
        rows in 65_536u32..=u32::MAX,
    ) {
        let shape = Shape::Table {
            cols: Dimension::Fixed(cols),
            rows: Dimension::Fixed(rows),
        };

        prop_assert_eq!(shape.element_count(), None);
    }

    /// Whatever the input, re-parsing the same text gives the same answer.
    #[test]
    fn parsing_is_deterministic(text in "\\PC{0,300}") {
        let first = parse(&text).map(|definition| definition.summary());
        let second = parse(&text).map(|definition| definition.summary());
        prop_assert_eq!(first, second);
    }

    /// A definition that parses has a signature: everything else follows the
    /// signature, so accepting a file without one would be a silent lie.
    ///
    /// The generator is the alphabet firmware signatures actually use. A `"`
    /// or `;` cannot round-trip through a quoted INI value at all — the
    /// dialect has no escape for them — so requiring one to survive would be
    /// testing the format's limits rather than the parser.
    #[test]
    fn a_parsed_definition_always_has_a_signature(signature in "[A-Za-z0-9 ._-]{0,40}") {
        let text = format!(
            "[MegaTune]\nsignature = \"{signature}\"\n[Constants]\nnPages = 1\npageSize = 64\npage = 1\nx = scalar, U08, 0, \"u\", 1, 0\n"
        );
        match parse(&text) {
            Ok(definition) => prop_assert_eq!(definition.signature, signature),
            Err(error) => {
                // Only an empty signature may be rejected, and it must say why.
                prop_assert!(signature.trim().is_empty(), "rejected a valid signature: {error}");
            }
        }
    }

    /// A generated label list keeps its text and its indexes. The generator
    /// stays inside what a quoted INI value can carry (no `"` or `,`), but it
    /// includes `=` and parentheses, the characters this parser used to split
    /// on by mistake.
    #[test]
    fn generated_bit_labels_keep_their_text(
        labels in prop::collection::vec("[A-Za-z][A-Za-z0-9 ()<>=!]{0,10}", 1..=8)
    ) {
        let mut text = String::from(
            "[MegaTune]\nsignature = \"x\"\n[Constants]\nnPages = 1\npageSize = 64\npage = 1\n#define opts = ",
        );
        for (index, label) in labels.iter().enumerate() {
            if index > 0 {
                text.push_str(", ");
            }
            text.push('"');
            text.push_str(label);
            text.push('"');
        }
        text.push_str("\nfield = bits, U08, 0, [0:7], $opts\n");

        let definition = parse(&text).expect("the generated definition parses");
        let options = &definition
            .constant("field")
            .expect("the field parses")
            .options;

        prop_assert_eq!(options.len(), labels.len());
        for (index, label) in labels.iter().enumerate() {
            prop_assert_eq!(&options[index].label, label);
            prop_assert_eq!(options[index].value, u32::try_from(index).expect("index fits"));
        }
    }

    /// The presentation attributes of a PC variable keep their positions, the
    /// same way a constant's do. A dropped scale turns `-1.5` into `-2.0` on
    /// the next save.
    #[test]
    fn generated_pc_variable_attributes_keep_their_positions(
        scale in any::<f64>().prop_filter("finite", |value| value.is_finite()),
        translate in any::<f64>().prop_filter("finite", |value| value.is_finite()),
        minimum in any::<f64>().prop_filter("finite", |value| value.is_finite()),
        maximum in any::<f64>().prop_filter("finite", |value| value.is_finite()),
        digits in 0u8..6,
    ) {
        let text = format!(
            "[MegaTune]\nsignature = \"x\"\n[PcVariables]\n\
             v = scalar, U16, \"u\", {scale}, {translate}, {minimum}, {maximum}, {digits}\n"
        );

        let definition = parse(&text).expect("the generated definition parses");
        let variable = definition.pc_variables.get("v").expect("the variable parses");

        let literal = |number: &Option<oxtuner_ini::Number>| {
            number.as_ref().and_then(oxtuner_ini::Number::literal)
        };
        prop_assert_eq!(literal(&variable.scale), Some(scale));
        prop_assert_eq!(literal(&variable.translate), Some(translate));
        prop_assert_eq!(literal(&variable.minimum), Some(minimum));
        prop_assert_eq!(literal(&variable.maximum), Some(maximum));
        prop_assert_eq!(literal(&variable.digits), Some(f64::from(digits)));
    }

    /// Arbitrary option-list text never panics, including quotes and periods:
    /// the separator scan walks bytes and must stay inside the field.
    #[test]
    fn arbitrary_option_text_never_panics(field in "[^\n]{0,200}") {
        let text = format!(
            "[MegaTune]\nsignature = \"x\"\n[Constants]\nnPages = 1\npageSize = 64\n\
             page = 1\nf = bits, U08, 0, [0:2], {field}\n"
        );

        let _ = parse(&text);
    }

    /// Arbitrary `[PcVariables]` text never panics: the shape and attribute
    /// split runs on user text, including a missing or extra offset.
    #[test]
    fn arbitrary_pc_variable_text_never_panics(
        class in prop::sample::select(vec!["array", "scalar", "bits", "string", "oddArray"]),
        rest in "[^\n]{0,140}",
    ) {
        // Pinning the class keeps the input past the `unknown-class` guard, so
        // the shape split, the offset decision and the attribute split run.
        let text = format!(
            "[MegaTune]\nsignature = \"x\"\n[PcVariables]\nv = {class}, U08, {rest}\n"
        );

        let _ = parse(&text);
    }
}
