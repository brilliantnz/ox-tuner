//! ISO-8859-1 (Latin-1) at the byte boundaries of the `TunerStudio` formats.
//!
//! Every format this application shares with `TunerStudio` is byte-oriented and
//! Latin-1: `.msq` files, text constants on the wire, and `project.properties`
//! (a Java properties file, read as ISO-8859-1). Rust strings are UTF-8, so the
//! conversion happens at those boundaries and nowhere else. Keeping it in one
//! crate means a rule that must stay identical — "a byte is the code point of
//! the same value", "a character above U+00FF is refused, never replaced" — is
//! written once.
//!
//! Two decoding policies live here and must not be confused:
//!
//! - [`decode_latin1`] is the strict, lossless one. A `.msq` or a page image is
//!   Latin-1 by declaration, so the same bytes must decode the same way every
//!   time, even when the file was hand-edited as UTF-8 and now looks like
//!   mojibake.
//! - [`decode_utf8_or_latin1`] is the forgiving one. A user's definition file
//!   can be UTF-8 with a stray Latin-1 byte in a help string; losing the whole
//!   file over that would make the definition unusable. Valid UTF-8 is kept,
//!   the undecodable bytes are read as Latin-1, and the caller is told that it
//!   happened so it can report once.
//!
//! Encoding is the lossy direction: a character above U+00FF has no Latin-1
//! byte, and both callers refuse to save rather than write `?` into a name or
//! a comment. [`escape_java_property`] is the one place where such a character
//! is representable: Java properties files escape it as `\uXXXX`, which stays
//! ASCII and therefore survives both ISO-8859-1 and UTF-8 readers.

#![forbid(unsafe_code)]

use std::fmt;

/// A character that ISO-8859-1 cannot represent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Latin1Error {
    /// The first such character, in source order.
    pub character: char,
    /// Its byte offset in the string.
    pub offset: usize,
}

impl fmt::Display for Latin1Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "character {:?} (at byte {}) cannot be written as ISO-8859-1",
            self.character, self.offset
        )
    }
}

impl std::error::Error for Latin1Error {}

/// Text decoded from bytes that may not all have been UTF-8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedText {
    /// The text, always valid UTF-8.
    pub text: String,
    /// Whether any byte sequence had to be read as Latin-1.
    pub had_invalid_utf8: bool,
}

/// Decodes ISO-8859-1 bytes losslessly.
///
/// Every byte maps to the code point of the same value, so encoding the result
/// gives the input back, byte for byte.
#[must_use]
pub fn decode_latin1(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| char::from(*byte)).collect()
}

/// Encodes text as ISO-8859-1, or reports the first character that does not fit.
///
/// # Errors
///
/// Returns [`Latin1Error`] naming the first character above U+00FF and its byte
/// offset.
pub fn encode_latin1(text: &str) -> Result<Vec<u8>, Latin1Error> {
    let mut bytes = Vec::with_capacity(text.len());
    for (offset, character) in text.char_indices() {
        let code = u32::from(character);
        bytes.push(u8::try_from(code).map_err(|_| Latin1Error { character, offset })?);
    }
    Ok(bytes)
}

/// The first character in `text` that has no ISO-8859-1 byte.
#[must_use]
pub fn first_non_latin1(text: &str) -> Option<char> {
    text.chars().find(|character| u32::from(*character) > 0xFF)
}

/// Drops a UTF-8 byte order mark, which would otherwise become part of the
/// first line and hide the first section or directive.
#[must_use]
pub fn without_utf8_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)
}

/// Decodes a definition file: UTF-8 where valid, Latin-1 for the remainder.
///
/// A byte order mark is not stripped here; [`without_utf8_bom`] is the caller's
/// explicit choice, because a BOM only matters to a line-oriented format.
///
/// Never fails. The caller reports [`DecodedText::had_invalid_utf8`] once
/// instead of failing the whole file, because a user-supplied definition must
/// stay usable when one help string carries an odd byte.
///
/// # Panics
///
/// Never in practice: the one `expect` asserts a `std::str::from_utf8` invariant
/// that `Utf8Error::valid_up_to` guarantees.
#[must_use]
pub fn decode_utf8_or_latin1(bytes: &[u8]) -> DecodedText {
    let Ok(text) = std::str::from_utf8(bytes) else {
        let mut text = String::with_capacity(bytes.len());
        let mut rest = bytes;
        while !rest.is_empty() {
            match std::str::from_utf8(rest) {
                Ok(tail) => {
                    text.push_str(tail);
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    // `valid_up_to` is always a UTF-8 boundary, so this slice
                    // cannot fail.
                    let head = std::str::from_utf8(&rest[..valid])
                        .expect("valid_up_to is a valid UTF-8 boundary");
                    text.push_str(head);
                    let invalid = error.error_len().unwrap_or(rest.len() - valid);
                    for byte in &rest[valid..valid + invalid] {
                        text.push(char::from(*byte));
                    }
                    rest = &rest[valid + invalid..];
                }
            }
        }

        return DecodedText {
            text,
            had_invalid_utf8: true,
        };
    };

    DecodedText {
        text: text.to_owned(),
        had_invalid_utf8: false,
    }
}

/// Escapes a value the way a Java properties file expects it.
///
/// `TunerStudio` loads `project.properties` as ISO-8859-1, so a UTF-8 byte
/// sequence reads back as mojibake. Everything outside ASCII is escaped as
/// `\uXXXX`, which both ISO-8859-1 and UTF-8 readers decode identically.
/// Backslashes and control characters are escaped for the same format, and so
/// is a leading space: the loader trims one that is not escaped.
#[must_use]
pub fn escape_java_property(value: &str) -> String {
    use std::fmt::Write as _;

    let mut escaped = String::with_capacity(value.len());
    for (index, character) in value.char_indices() {
        match character {
            // The loader trims a leading space, so the first one is escaped.
            ' ' if index == 0 => escaped.push_str("\\ "),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            // Any other C0 or DEL byte would be read differently by the two
            // loaders, so it is escaped like a non-ASCII character.
            character if character.is_ascii_control() => {
                let _ = write!(escaped, "\\u{:04X}", u32::from(character));
            }
            character if character.is_ascii() => escaped.push(character),
            character => {
                let mut units = [0_u16; 2];
                for unit in character.encode_utf16(&mut units) {
                    // Writing to a `String` cannot fail, so the result is
                    // `fmt::Error` only in theory.
                    let _ = write!(escaped, "\\u{unit:04X}");
                }
            }
        }
    }

    escaped
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "unit tests may unwrap; a failed call is a failed test"
    )]
    use super::*;

    #[test]
    fn every_byte_round_trips() {
        let bytes: Vec<u8> = (0..=255).collect();
        assert_eq!(encode_latin1(&decode_latin1(&bytes)).unwrap(), bytes);
    }

    #[test]
    fn text_above_latin1_is_reported_not_replaced() {
        let error = encode_latin1("café → ok").expect_err("the arrow is not Latin-1");
        assert_eq!(error.character, '→');
        assert_eq!(error.offset, "café ".len());
        assert_eq!(encode_latin1("café").unwrap(), b"caf\xe9");
    }

    #[test]
    fn latin1_is_a_predicate() {
        assert_eq!(first_non_latin1("café ±"), None);
        assert_eq!(first_non_latin1("café →"), Some('→'));
        assert_eq!(first_non_latin1(""), None);
    }

    #[test]
    fn empty_text_round_trips() {
        assert_eq!(decode_latin1(b""), "");
        assert_eq!(encode_latin1("").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn a_definition_decodes_utf8_or_latin1_without_failing() {
        let plain = decode_utf8_or_latin1(b"[Constants]\n");
        assert_eq!(plain.text, "[Constants]\n");
        assert!(!plain.had_invalid_utf8);

        // `0xB1` is the real rusEFI case: `±` written as Latin-1 inside an
        // otherwise UTF-8 file.
        let mut bytes = b"units = \"".to_vec();
        bytes.push(0xB1);
        bytes.extend_from_slice(b"\"\n");
        let mixed = decode_utf8_or_latin1(&bytes);
        assert!(mixed.text.contains('±'));
        assert!(mixed.had_invalid_utf8);
    }

    #[test]
    fn a_byte_order_mark_can_be_dropped() {
        let bytes = b"\xEF\xBB\xBF[MegaTune]\n";
        assert_eq!(without_utf8_bom(bytes), b"[MegaTune]\n");
        assert_eq!(without_utf8_bom(b"[MegaTune]\n"), b"[MegaTune]\n");
    }

    #[test]
    fn a_property_value_stays_ascii_and_escapes_the_rest() {
        assert_eq!(escape_java_property("Roadster"), "Roadster");
        assert_eq!(escape_java_property("Voiture é"), "Voiture \\u00E9");
        assert_eq!(escape_java_property("back\\slash"), "back\\\\slash");
        assert_eq!(escape_java_property("tab\there"), "tab\\there");
        assert_eq!(
            escape_java_property(" leading"),
            "\\ leading",
            "the loader trims an unescaped leading space"
        );
        assert_eq!(
            escape_java_property("\u{1}bell\u{7}"),
            "\\u0001bell\\u0007",
            "a control character outside the named escapes is escaped too"
        );
        assert_eq!(
            escape_java_property("🚗"),
            "\\uD83D\\uDE97",
            "a character outside the BMP is a surrogate pair"
        );
    }
}
