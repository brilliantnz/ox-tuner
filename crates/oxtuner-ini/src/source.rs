//! Decoding INI bytes into text.
//!
//! Real definitions are not reliably UTF-8: rusEFI's generated INI carries a
//! single raw `0xB1` (`±`) inside a help string, and `str::from_utf8` rejects
//! the whole file because of it. A definition the
//! user owns must never become unusable because one byte is odd, so decoding is
//! lossy — UTF-8 where it is valid, Latin-1 for the remainder — and the loss is
//! reported once, not per byte.
//!
//! The byte-mixing rule lives in `oxtuner-encoding` with the other ISO-8859-1
//! boundaries; this module only adds the INI-shaped diagnostic.

use oxtuner_encoding::decode_utf8_or_latin1;

use crate::error::{Diagnostic, Severity};

/// Decoded text plus whether anything had to be reinterpreted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    /// The file as text, valid UTF-8.
    pub text: String,
    /// `Some` when bytes were not valid UTF-8 and Latin-1 was used for them.
    pub diagnostic: Option<Diagnostic>,
}

/// Decodes a definition file, never failing.
///
/// A UTF-8 BOM is dropped: it would otherwise become part of the first line and
/// hide the first `[section]` or `#directive`.
pub fn decode(bytes: &[u8]) -> Decoded {
    let bytes = oxtuner_encoding::without_utf8_bom(bytes);
    let decoded = decode_utf8_or_latin1(bytes);

    Decoded {
        text: decoded.text,
        diagnostic: decoded.had_invalid_utf8.then(|| {
            Diagnostic::whole_file(
                Severity::Note,
                "non-utf8",
                "the file is not valid UTF-8; undecodable bytes were read as Latin-1",
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_utf8_passes_through_with_no_diagnostic() {
        let decoded = decode(b"[Constants]\nendianness = little\n");

        assert_eq!(decoded.text, "[Constants]\nendianness = little\n");
        assert!(decoded.diagnostic.is_none());
    }

    #[test]
    fn a_single_bad_byte_does_not_lose_the_file() {
        // `0xB1` is the real rusEFI case: `±` written as Latin-1 inside an
        // otherwise UTF-8 file.
        let mut bytes = b"units = \"".to_vec();
        bytes.push(0xB1);
        bytes.extend_from_slice(b"\"\n[Constants]\n");

        let decoded = decode(&bytes);

        assert!(decoded.text.contains('±'));
        assert!(decoded.text.contains("[Constants]"));
        assert_eq!(
            decoded.diagnostic.as_ref().map(|d| d.severity),
            Some(Severity::Note)
        );
    }

    #[test]
    fn a_byte_order_mark_is_dropped_so_the_first_line_still_parses() {
        let decoded = decode(b"\xEF\xBB\xBF[MegaTune]\nsignature = \"x\"\n");

        assert!(decoded.text.starts_with("[MegaTune]"));
    }

    #[test]
    fn latin1_text_round_trips_every_byte() {
        let bytes: Vec<u8> = (0x80..=0xFF).collect();

        let decoded = decode(&bytes);

        assert_eq!(decoded.text.chars().count(), bytes.len());
    }
}
