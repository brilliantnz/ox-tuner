//! ISO-8859-1 (Latin-1) at the `.msq` byte boundary.
//!
//! TunerStudio writes `.msq` files as ISO-8859-1, not UTF-8. Decoding each
//! byte as the code point of the same value is lossless (`byte → U+0000..U+00FF`),
//! so a value read from a file survives a round-trip unchanged.
//!
//! Encoding is the lossy direction: a character above U+00FF — which can only
//! come from something the user typed or pasted into the UI — has no Latin-1
//! byte. It is reported ([`EncodeError`]) rather than replaced with `?`, because
//! silently corrupting a name or a comment is worse than refusing the save.

use std::fmt;

/// A character that ISO-8859-1 cannot represent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeError {
    /// The first such character, in source order.
    pub character: char,
    /// Its byte offset in the string.
    pub offset: usize,
}

impl fmt::Display for EncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "character {:?} (at byte {}) cannot be written as ISO-8859-1",
            self.character, self.offset
        )
    }
}

impl std::error::Error for EncodeError {}

/// Decodes ISO-8859-1 bytes losslessly.
pub fn decode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| char::from(*byte)).collect()
}

/// Encodes text as ISO-8859-1, or reports the first character that does not fit.
pub fn encode(text: &str) -> Result<Vec<u8>, EncodeError> {
    let mut bytes = Vec::with_capacity(text.len());
    for (offset, character) in text.char_indices() {
        let code = u32::from(character);
        bytes.push(u8::try_from(code).map_err(|_| EncodeError { character, offset })?);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_byte_round_trips() {
        let bytes: Vec<u8> = (0..=255).collect();
        assert_eq!(encode(&decode(&bytes)).unwrap(), bytes);
    }

    #[test]
    fn text_above_latin1_is_reported_not_replaced() {
        let error = encode("café → ok").expect_err("the arrow is not Latin-1");
        assert_eq!(error.character, '→');
        assert_eq!(error.offset, "café ".len());
        assert_eq!(encode("café").unwrap(), b"caf\xe9");
    }
}
