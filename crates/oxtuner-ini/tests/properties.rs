//! Property tests: the parser must be total.
//!
//! The contract is narrow and absolute — **any bytes produce either a
//! definition or a structured error, never a panic, never a hang**. Fuzzing
//! covers the same ground with coverage guidance; these tests run everywhere,
//! including the three-OS CI matrix, so a regression cannot depend on someone
//! remembering to run `cargo fuzz`.

use proptest::prelude::*;

use oxtuner_ini::{
    Dimension, IniError, Number, SettingsContext, Shape, Text, parse_bytes, parse_str, preprocess,
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

    /// Splitting one `[Constants]` block around a neutral section must not
    /// change the constants it produces. The second half uses `nextOffset`, so
    /// the page cursor has to survive the split for the two models to match.
    #[test]
    fn a_split_constants_block_produces_the_same_model(
        offsets in proptest::collection::vec(0u32..4096, 1..12),
    ) {
        let header = "[MegaTune]\nsignature = \"x\"\n[Constants]\n\
                      nPages = 1\npageSize = 4096\npage = 1\n";
        let midpoint = offsets.len() / 2;
        let mut whole = String::from(header);
        let mut split = String::from(header);

        for (index, offset) in offsets.iter().enumerate() {
            let written = if index < midpoint {
                offset.to_string()
            } else {
                "nextOffset".to_owned()
            };
            let line = format!("c_{index} = scalar, U08, {written}, \"u\", 1, 0, 0, 255, 0\n");
            whole.push_str(&line);
            if index == midpoint {
                split.push_str("[OutputChannels]\n[Constants]\n");
            }
            split.push_str(&line);
        }

        let whole = parse(&whole).expect("whole block parses");
        let split = parse(&split).expect("split block parses");
        prop_assert_eq!(whole.constants.len(), split.constants.len());
        for (name, constant) in whole.constants.iter() {
            let other = split.constant(name).expect("both blocks declare it");
            prop_assert_eq!(
                constant.address,
                other.address,
                "offsets diverged for {}",
                name
            );
        }
        prop_assert_eq!(whole.pages.len(), split.pages.len());
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

    /// `Display` round-trips the source of an attribute expression, so printing
    /// a parsed attribute and parsing it again must rebuild the same tree.
    #[test]
    fn attribute_expressions_survive_print_parse_print(
        expression in prop::sample::select(vec![
            "1",
            "1 + 2",
            "fuel_cols",
            "fuel_cols * 2",
            "min(a, b)",
            "max(a, b) + 1",
            "abs(-3)",
            "sqrt(4)",
            "a ? b : c",
            "a && b || c",
            "array.foo[3]",
            "getChannelScaleByOffset(6)",
            "bitStringValue(units, algo)",
        ])
    ) {
        let source = format!("{{{expression}}}");

        let number = Number::parse(&source);
        let printed = number.to_string();
        let reparsed = Number::parse(&printed);
        prop_assert_eq!(&reparsed, &number);
        prop_assert_eq!(reparsed.to_string(), printed);

        let text = Text::parse(&source);
        let printed = text.to_string();
        let reparsed = Text::parse(&printed);
        prop_assert_eq!(&reparsed, &text);
        prop_assert_eq!(reparsed.to_string(), printed);
    }

    /// Generated expression-shaped strings, including ones the parser must
    /// reject, still round-trip whenever they do parse.
    #[test]
    fn generated_attribute_expressions_survive_print_parse_print(
        body in "[A-Za-z0-9_+*/.(){}, $ -]{1,80}"
    ) {
        let source = format!("{{{body}}}");
        let number = Number::parse(&source);
        if let Number::Expression { parsed: None, .. } = number {
            return Ok(());
        }
        let printed = number.to_string();
        let reparsed = Number::parse(&printed);
        prop_assert_eq!(&reparsed, &number);
        prop_assert_eq!(reparsed.to_string(), printed);
    }

    /// Commas inside quotes or braces never split fields: a quoted label and
    /// a brace expression each stay one field no matter what they carry.
    #[test]
    fn commas_inside_quotes_or_braces_never_split(
        outer_a in "[A-Za-z0-9_]{1,12}",
        quoted in "[A-Za-z0-9_ =;]{1,20}",
        braced in "[A-Za-z0-9_ =;]{1,20}",
        outer_b in "[A-Za-z0-9_]{1,12}",
    ) {
        let value = format!("{outer_a}, \"{quoted}\", {{{braced}}}, {outer_b}");
        let expected_quoted = format!("\"{quoted}\"");
        let expected_braced = format!("{{{braced}}}");

        let fields = oxtuner_ini::fields::split_fields(&value);

        prop_assert_eq!(fields.len(), 4);
        prop_assert_eq!(fields[0], outer_a.as_str());
        prop_assert_eq!(fields[1], expected_quoted.as_str());
        prop_assert_eq!(fields[2], expected_braced.as_str());
        prop_assert_eq!(fields[3], outer_b.as_str());
    }

    /// Balanced nested braces of any depth never panic and never split: the
    /// whole expression stays one field, however deep it nests.
    #[test]
    fn nested_braces_of_any_depth_keep_fields_intact(depth in 1usize..32) {
        let mut expression = String::from("a, b");
        for _ in 0..depth {
            expression.insert(0, '{');
            expression.push('}');
        }
        let value = format!("before, {expression}, after");

        let fields = oxtuner_ini::fields::split_fields(&value);

        prop_assert_eq!(fields.len(), 3);
        prop_assert_eq!(fields[0], "before");
        prop_assert_eq!(fields[1], expression.as_str());
        prop_assert_eq!(fields[2], "after");
    }

    /// Empty fields are dropped by `split_fields` and kept by
    /// `split_fields_preserving_empty`: the positions of the kept fields
    /// must survive, gaps included.
    #[test]
    fn empty_fields_survive_only_when_kept(
        first in "[A-Za-z0-9_]{1,10}",
        last in "[A-Za-z0-9_]{1,10}",
        gaps in 1usize..4,
    ) {
        let mut value = first.clone();
        for _ in 0..gaps {
            value.push(',');
        }
        value.push_str(&last);

        let dropped = oxtuner_ini::fields::split_fields(&value);
        let kept = oxtuner_ini::fields::split_fields_preserving_empty(&value);

        prop_assert_eq!(dropped, vec![first.as_str(), last.as_str()]);
        prop_assert_eq!(kept.len(), gaps + 1);
        prop_assert_eq!(kept[0], first.as_str());
        prop_assert_eq!(kept[kept.len() - 1], last.as_str());
        prop_assert!(kept[1..kept.len() - 1].iter().all(|field| field.is_empty()));
    }

    /// `=` inside a quoted label or a brace expression never splits the name:
    /// only the first top-level `=` separates the key from the value.
    #[test]
    fn equals_inside_quotes_or_braces_never_splits_the_name(
        name in "[A-Za-z0-9_]{1,12}",
        left in "[A-Za-z0-9_]{1,10}",
        right in "[A-Za-z0-9_]{1,10}",
    ) {
        let quoted = format!("{name} = \"{left}={right}\"");
        let expected_quoted = format!("\"{left}={right}\"");

        let (key, value) = oxtuner_ini::fields::split_assignment(&quoted)
            .expect("the top-level equals splits the line");

        prop_assert_eq!(key, name.as_str());
        prop_assert_eq!(value, expected_quoted.as_str());

        let braced = format!("{name} = {{{left}={right}}}");
        let expected_braced = format!("{{{left}={right}}}");

        let (key, value) = oxtuner_ini::fields::split_assignment(&braced)
            .expect("the top-level equals splits the line");

        prop_assert_eq!(key, name.as_str());
        prop_assert_eq!(value, expected_braced.as_str());
    }

    /// A `;` inside quotes is data, not a comment: a trailing comment is
    /// stripped only outside quotes, while a key that intentionally contains
    /// `;` keeps it.
    #[test]
    fn semicolons_inside_quotes_are_kept_while_trailing_comments_go(
        name in "[A-Za-z0-9_]{1,12}",
        inside in "[A-Za-z0-9_ =]{1,16}",
        comment in "[A-Za-z0-9_ ]{1,16}",
    ) {
        let line = format!("{name} = \"{inside};still quoted\" ; {comment}");
        let expected = format!("{name} = \"{inside};still quoted\" ");

        prop_assert_eq!(
            oxtuner_ini::fields::strip_comment(&line),
            expected.as_str()
        );

        let quoted_key = format!("\"{inside};{name}\" = 1");

        prop_assert_eq!(
            oxtuner_ini::fields::strip_comment(&quoted_key),
            quoted_key.as_str()
        );
    }

    /// Every byte from `0x80` to `0xFF` decodes as Latin-1 without a
    /// replacement character, and raises exactly one `non-utf8` note.
    #[test]
    fn latin1_bytes_decode_without_replacements(byte in 0x80u8..=0xFF) {
        let decoded = oxtuner_ini::source::decode(&[byte]);
        let expected = char::from(byte).to_string();

        prop_assert_eq!(decoded.text.as_str(), expected.as_str());
        prop_assert!(
            !decoded.text.contains('\u{FFFD}'),
            "byte {byte:#04X} must not become a replacement character"
        );

        let diagnostic = decoded.diagnostic.expect("one non-utf8 note per file");
        let code = diagnostic.code.into_owned();

        prop_assert_eq!(code.as_str(), "non-utf8");
    }

    /// Valid UTF-8 passes through untouched with no note: decoding tries
    /// UTF-8 first and only falls back when it must.
    #[test]
    fn valid_utf8_passes_through_with_no_note(
        text in "\\PC{0,200}".prop_filter("no leading BOM", |text: &String| !text.starts_with('\u{FEFF}'))
    ) {
        let decoded = oxtuner_ini::source::decode(text.as_bytes());

        prop_assert_eq!(decoded.text.as_str(), text.as_str());
        prop_assert!(decoded.diagnostic.is_none(), "valid UTF-8 needs no note");
    }

    /// A byte order mark is stripped before decoding, so it never becomes
    /// part of the first line and never counts as a decoding failure.
    #[test]
    fn a_byte_order_mark_is_stripped_before_decoding(text in "[A-Za-z0-9_ ]{1,60}") {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(text.as_bytes());

        let decoded = oxtuner_ini::source::decode(&bytes);

        prop_assert_eq!(decoded.text.as_str(), text.as_str());
        prop_assert!(decoded.diagnostic.is_none(), "a BOM alone is not a failure");
    }
}
