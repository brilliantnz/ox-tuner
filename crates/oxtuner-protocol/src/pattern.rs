//! A command template turned into a matcher.
//!
//! The simulator has to understand the commands a definition declares without
//! hard-coding Speeduino's letters. [`CommandPattern`] is that bridge: it reads
//! the same [`oxtuner_ini::CommandTemplate`] the protocol encodes with and recognises
//! the bytes on the wire, extracting the page, offset, count and payload.
//!
//! ```text
//! "p%2i%2o%2c"  ──▶  ['p'] [page: u8, u8] [offset: u16] [count: u16]
//! ```

use oxtuner_ini::{CommandTemplate, Placeholder, Segment, WireOrder};

use crate::error::ProtocolError;

/// One recognised piece of a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    /// A literal byte.
    Byte(u8),
    /// `%2i`: the `[canId][page]` pair.
    PageId,
    /// `%2o`: a 16-bit offset.
    Offset,
    /// `%2c`: a 16-bit count.
    Count,
    /// `%v` / `%2v`: the payload bytes.
    Values,
}

/// A command template that can be matched against wire bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPattern {
    parts: Vec<Part>,
    wire_order: WireOrder,
}

/// What a matched command carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatternMatch<'a> {
    /// The raw two-byte page identifier, for commands with `%2i`.
    ///
    /// Definitions are not consistent about the second byte: Speeduino writes
    /// `[canId][page]`, while a board-generated INI may write a little-endian
    /// word. Comparing the raw pair is the only correct test.
    pub identifier: Option<[u8; 2]>,
    /// The page byte, for commands with `%2i`.
    pub page: Option<u8>,
    /// The 16-bit offset, for commands with `%2o`.
    pub offset: u16,
    /// The 16-bit count, for commands with `%2c`.
    pub count: u16,
    /// The payload, for commands with `%v`/`%2v`.
    pub values: &'a [u8],
}

impl CommandPattern {
    /// Builds a pattern from a definition command.
    ///
    /// Returns `None` when the command carries text this build does not model
    /// (an [`Segment::Unparsed`]) or a `$variable` the resolver cannot give a
    /// value: guessing either would make the simulator answer a command the
    /// firmware would not.
    pub fn new(command: &CommandTemplate, variables: &dyn Fn(&str) -> Option<u8>) -> Option<Self> {
        let mut parts = Vec::with_capacity(command.segments.len());
        for segment in &command.segments {
            match segment {
                Segment::Byte(byte) => parts.push(Part::Byte(*byte)),
                Segment::Variable(name) => parts.push(Part::Byte(variables(name)?)),
                Segment::Placeholder(Placeholder::PageId) => parts.push(Part::PageId),
                Segment::Placeholder(Placeholder::Offset) => parts.push(Part::Offset),
                Segment::Placeholder(Placeholder::Count) => parts.push(Part::Count),
                Segment::Placeholder(Placeholder::Values | Placeholder::Value) => {
                    parts.push(Part::Values);
                }
                Segment::Unparsed(_) => return None,
            }
        }
        if parts.is_empty() {
            return None;
        }
        Some(Self {
            parts,
            wire_order: WireOrder::default(),
        })
    }

    /// Fixes the wire order of the 16-bit arguments.
    #[must_use]
    pub fn with_wire_order(mut self, order: WireOrder) -> Self {
        self.wire_order = order;
        self
    }

    /// Whether this pattern can describe `bytes` at all.
    pub fn matches(&self, bytes: &[u8]) -> bool {
        self.capture(bytes).is_some()
    }

    /// Extracts the arguments from a complete command.
    pub fn capture<'a>(&self, bytes: &'a [u8]) -> Option<PatternMatch<'a>> {
        let mut at = 0usize;
        let mut identifier = None;
        let mut offset = 0u16;
        let mut count = 0u16;
        let mut values: &[u8] = &[];

        for (index, part) in self.parts.iter().enumerate() {
            match part {
                Part::Byte(expected) => {
                    if *bytes.get(at)? != *expected {
                        return None;
                    }
                    at += 1;
                }
                Part::PageId => {
                    // The first byte is the CAN id on Speeduino, but a
                    // board-generated INI may use both bytes as a word; the raw
                    // pair is what identifies the page.
                    let pair = [*bytes.get(at)?, *bytes.get(at + 1)?];
                    identifier = Some(pair);
                    at += 2;
                }
                Part::Offset => {
                    offset = self.word(bytes.get(at..at + 2)?);
                    at += 2;
                }
                Part::Count => {
                    count = self.word(bytes.get(at..at + 2)?);
                    at += 2;
                }
                Part::Values => {
                    let remaining = bytes.len().checked_sub(at)?;
                    // A page-value command writes a fixed number of bytes: the
                    // count says how many, and a payload placeholder without a
                    // count takes whatever is left.
                    let take = if count == 0 {
                        remaining
                    } else {
                        usize::from(count)
                    };
                    if take > remaining {
                        return None;
                    }
                    values = &bytes[at..at + take];
                    at += take;
                    // A payload placeholder is always last in a real command;
                    // if it is not, the template is not one this matcher can
                    // read safely.
                    if index + 1 != self.parts.len() {
                        return None;
                    }
                }
            }
        }

        (at == bytes.len()).then_some(PatternMatch {
            identifier,
            page: identifier.map(|pair| pair[1]),
            offset,
            count,
            values,
        })
    }

    fn word(&self, bytes: &[u8]) -> u16 {
        let pair = [bytes[0], bytes[1]];
        match self.wire_order {
            WireOrder::LittleEndian => u16::from_le_bytes(pair),
            WireOrder::BigEndian => u16::from_be_bytes(pair),
        }
    }
}

/// The two bytes `%2i` expands to for one page.
///
/// The dialect lets a page declare its identifier (`pageIdentifier`), and that
/// is what a host substitutes. Speeduino writes `[tsCanId][page]`; a
/// board-generated INI may write two fixed bytes or a little-endian word (rusEFI
/// numbers pages from zero on the wire). This returns the **raw pair**, so a
/// caller never has to know which spelling the definition chose.
///
/// A page that omits the declaration falls back to `[0][page]`, the dialect's
/// own default, and never invents a CAN id.
///
/// # Errors
///
/// Returns [`ProtocolError::Encoding`] when the declared identifier cannot be
/// encoded, or [`ProtocolError::Unsupported`] when it does not encode to
/// exactly two bytes. A declared identifier is never replaced by the default:
/// doing so would send a page selection the definition did not ask for.
pub fn page_identifier(
    number: u8,
    identifier: Option<&oxtuner_ini::CommandTemplate>,
    variables: &dyn Fn(&str) -> Option<u8>,
) -> Result<[u8; 2], ProtocolError> {
    let Some(command) = identifier else {
        return Ok([0, number]);
    };
    let encoded = command
        .encode(&oxtuner_ini::CommandArguments::default(), variables)
        .map_err(ProtocolError::Encoding)?;
    match encoded.as_slice() {
        [first, second] => Ok([*first, *second]),
        other => Err(ProtocolError::Unsupported(format!(
            "the page identifier encodes to {} byte(s); the wire needs exactly two",
            other.len()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use oxtuner_ini::CommandTemplate;

    use super::*;

    fn pattern(text: &str) -> CommandPattern {
        CommandPattern::new(&CommandTemplate::parse(text), &|_| Some(0)).expect("a pattern")
    }

    #[test]
    fn a_page_read_yields_its_page_offset_and_count() {
        let command = CommandTemplate::parse(r#""p%2i%2o%2c""#);
        let pattern = CommandPattern::new(&command, &|_| Some(0)).expect("a pattern");

        let matched = pattern
            .capture(&[0x70, 0x00, 0x07, 0x20, 0x00, 0x10, 0x00])
            .expect("matches");

        assert_eq!(matched.page, Some(7));
        assert_eq!(matched.offset, 32);
        assert_eq!(matched.count, 16);
    }

    #[test]
    fn a_write_carries_its_payload() {
        let matched = pattern(r#""M%2i%2o%2c%v""#)
            .capture(&[0x4D, 0x00, 0x02, 0x00, 0x00, 0x02, 0x00, 0xAA, 0xBB])
            .expect("matches");

        assert_eq!(matched.page, Some(2));
        assert_eq!(matched.values, &[0xAA, 0xBB]);
    }

    #[test]
    fn a_short_command_is_not_a_match() {
        assert!(
            pattern(r#""p%2i%2o%2c""#)
                .capture(&[0x70, 0, 1, 0])
                .is_none()
        );
    }

    #[test]
    fn a_big_endian_builder_reads_the_word_in_wire_order() {
        let matched = pattern(r#""p%2i%2o%2c""#)
            .with_wire_order(WireOrder::BigEndian)
            .capture(&[0x70, 0x00, 0x07, 0x20, 0x00, 0x10, 0x00])
            .expect("matches");

        assert_eq!(matched.offset, 0x2000);
        assert_eq!(matched.count, 0x1000);
    }

    #[test]
    fn a_declared_page_identifier_must_encode_to_two_bytes() {
        // `%2o` is a 16-bit field, so this identifier is four bytes wide.
        let command = CommandTemplate::parse(r#""%2i%2o""#);

        let error = page_identifier(1, Some(&command), &|_| Some(0))
            .expect_err("the definition is wrong, and the default is not substituted");

        assert!(matches!(error, ProtocolError::Unsupported(_)), "{error}");
        assert!(error.to_string().contains("exactly two"), "{error}");
    }

    #[test]
    fn a_missing_page_identifier_takes_the_dialect_default() {
        assert_eq!(page_identifier(3, None, &|_| Some(9)), Ok([0, 3]));
    }
}
