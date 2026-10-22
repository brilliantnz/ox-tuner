//! Property tests: the parser must be total.
//!
//! The contract is narrow and absolute — **any bytes produce either a
//! definition or a structured error, never a panic, never a hang**. Fuzzing
//! covers the same ground with coverage guidance; these tests run everywhere,
//! including the three-OS CI matrix, so a regression cannot depend on someone
//! remembering to run `cargo fuzz`.

use proptest::prelude::*;

use oxtuner_ini::{IniError, SettingsContext, parse_bytes, parse_str, preprocess};

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
}
