//! Small tokenizer helpers shared by every section parser.
//!
//! Three dialect rules shape this module:
//!
//! - a `;` starts a comment **unless it is inside a quoted string**, so
//!   `versionInfo = "S" ; shown in the title bar` keeps its value;
//! - a comma inside `{ … }` or `"…"` does not split fields, or
//!   `{ bitStringValue(units, algorithm) }` becomes two fields;
//! - field *counts* must never be assumed — trailing fields are optional.
//!
//! Whitespace is ASCII, so every trim in this module is [`str::trim_ascii`].
//! A no-break space is data, not padding, and is kept as part of the field.

use std::fmt;

/// One command character as a byte.
#[expect(
    clippy::as_conversions,
    reason = "command strings are byte-oriented; a character keeps its low byte"
)]
fn command_byte(character: char) -> u8 {
    character as u8
}

/// Two hexadecimal digits as one byte.
fn hex_byte(high: u32, low: u32) -> u8 {
    u8::try_from(high * 16 + low).expect("two hexadecimal digits fit in one byte")
}

/// Removes a trailing `;` comment, respecting quoted strings.
///
/// The scan is byte-wise. Every delimiter the dialect uses is ASCII, and a
/// UTF-8 continuation byte is never an ASCII byte, so a byte can only match
/// where the matching character starts.
pub fn strip_comment(line: &str) -> &str {
    let mut in_quotes = false;
    let mut escaped = false;

    for (index, &byte) in line.as_bytes().iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match byte {
            b'\\' if in_quotes => escaped = true,
            b'"' => in_quotes = !in_quotes,
            b';' if !in_quotes => return &line[..index],
            _ => {}
        }
    }

    line
}

/// Splits `key = value` at the first `=` outside quotes and braces.
///
/// Returns `None` when the line has no assignment, which is how the parser
/// distinguishes entries from stray text.
pub fn split_assignment(line: &str) -> Option<(&str, &str)> {
    let mut in_quotes = false;
    let mut escaped = false;
    let mut depth = 0u32;

    for (index, &byte) in line.as_bytes().iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match byte {
            b'\\' if in_quotes => escaped = true,
            b'"' => in_quotes = !in_quotes,
            b'{' if !in_quotes => depth += 1,
            b'}' if !in_quotes => depth = depth.saturating_sub(1),
            b'=' if !in_quotes && depth == 0 => {
                return Some((line[..index].trim_ascii(), line[index + 1..].trim_ascii()));
            }
            _ => {}
        }
    }

    None
}

/// Whether the line contains a `=` outside quotes and braces.
pub fn has_assignment(line: &str) -> bool {
    split_assignment(line).is_some()
}

/// Splits a comma-separated value into trimmed fields, ignoring commas inside
/// `"…"` and `{ … }`. Empty fields are dropped.
///
/// The fields borrow `value`; a caller that stores them converts at the point
/// of storage. A definition has tens of thousands of fields, so copying each
/// one was a measurable share of a parse.
pub fn split_fields(value: &str) -> Vec<&str> {
    split(value, false)
}

/// [`split_fields`] without dropping empty fields.
///
/// Command template lists need this: rusEFI declares `burnCommand = "B%2i", "", ""`,
/// where the empty strings mark pages that cannot be burned. Dropping them
/// would shift every later page onto the wrong command.
pub fn split_fields_preserving_empty(value: &str) -> Vec<&str> {
    split(value, true)
}

/// The one scanner behind both [`split_fields`] and
/// [`split_fields_preserving_empty`]; trimming happens once, on the way out.
fn split(value: &str, keep_empty: bool) -> Vec<&str> {
    let mut fields = Vec::new();
    let mut start = 0usize;
    let mut in_quotes = false;
    let mut escaped = false;
    let mut depth = 0u32;

    for (index, &byte) in value.as_bytes().iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match byte {
            b'\\' if in_quotes => escaped = true,
            b'"' => in_quotes = !in_quotes,
            b'{' if !in_quotes => depth += 1,
            b'}' if !in_quotes => depth = depth.saturating_sub(1),
            b',' if !in_quotes && depth == 0 => {
                push_field(&mut fields, &value[start..index], keep_empty);
                start = index + 1;
            }
            _ => {}
        }
    }
    push_field(&mut fields, &value[start..], keep_empty);

    fields
}

/// Adds one trimmed field.
fn push_field<'a>(fields: &mut Vec<&'a str>, field: &'a str, keep_empty: bool) {
    let field = field.trim_ascii();
    if keep_empty || !field.is_empty() {
        fields.push(field);
    }
}

/// Removes one layer of double quotes, if present.
pub fn unquote(value: &str) -> &str {
    let value = value.trim_ascii();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

/// Whether a field is a `{ … }` expression.
pub fn is_expression_field(value: &str) -> bool {
    let value = value.trim_ascii();
    value.starts_with('{') && value.ends_with('}') && value.len() >= 2
}

/// The body of a `{ … }` expression field, without the braces.
pub fn expression_body(value: &str) -> Option<&str> {
    is_expression_field(value)
        .then(|| value.trim_ascii()[1..value.trim_ascii().len() - 1].trim_ascii())
}

/// Parses an integer, accepting decimal and `0x` hex (iniSpec 3.50).
pub fn parse_u32(value: &str) -> Option<u32> {
    let value = value.trim_ascii();
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        return u32::from_str_radix(hex, 16).ok();
    }
    value.parse().ok()
}

/// Parses a float, accepting integers, `0x` hex and a leading `+`.
pub fn parse_f64(value: &str) -> Option<f64> {
    let value = value.trim_ascii();
    if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        return u32::from_str_radix(hex, 16).ok().map(f64::from);
    }
    value.parse().ok()
}

/// A command template turned into segments the protocol layer can encode.
///
/// Dialect grammar: literal ASCII bytes, `\xHH` escapes, `$name` substitutions
/// from PC variables, and the `%` placeholders. Anything else is kept as
/// [`Segment::Unparsed`] so a definition we only partly understand still loads
/// and can be reported precisely.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandTemplate {
    /// The command as written, minus the surrounding quotes.
    pub raw: String,
    /// The pieces, in wire order.
    pub segments: Vec<Segment>,
}

/// Why a command template could not be encoded.
///
/// The message names the segment that failed; callers that only display the
/// failure can use [`Display`](fmt::Display), and a caller that needs the
/// underlying cause can walk [`std::error::Error::source`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeError(String);

impl fmt::Display for EncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for EncodeError {}

impl From<String> for EncodeError {
    fn from(message: String) -> Self {
        Self(message)
    }
}

/// One piece of a command string.
#[expect(
    clippy::exhaustive_enums,
    reason = "the command grammar has a fixed set of segment kinds"
)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// One byte with a known value.
    Byte(u8),
    /// A `$name` / `\$name` reference to a PC variable.
    Variable(String),
    /// `%2i`, `%2o`, `%2c`, `%2v` or `%v`.
    Placeholder(Placeholder),
    /// Text this build does not understand; kept for diagnostics.
    Unparsed(String),
}

/// The parameter placeholders the dialect defines.
#[expect(
    clippy::exhaustive_enums,
    reason = "the INI dialect defines exactly these parameter placeholders"
)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placeholder {
    /// `%2i` — page identifier, two bytes: the `[canId][page]` pair, written
    /// big-endian. It is read byte-wise on the wire (canId first), so this is
    /// a composite, not an endianness question.
    PageId,
    /// `%2o` — start offset, two bytes. Little-endian on the wire unless the
    /// definition sets `forceBigEndianProtocol`.
    Offset,
    /// `%2c` — byte count, two bytes. Same wire order as [`Self::Offset`].
    Count,
    /// `%2v` — the payload byte array of `%2c` bytes.
    Values,
    /// `%v` — the payload bytes of one write.
    ///
    /// The dialect describes it as one value; in practice the same keyword is
    /// used for single-byte and word writes (`M%2i%2o%2c%v` writes two bytes at
    /// a time in the `MegaSquirt` corpus), so the encoder emits every payload
    /// byte and lets `%2c` state the count.
    Value,
}

impl fmt::Display for Placeholder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::PageId => "%2i",
            Self::Offset => "%2o",
            Self::Count => "%2c",
            Self::Values => "%2v",
            Self::Value => "%v",
        })
    }
}

impl CommandTemplate {
    /// Parses a quoted or bare command string.
    pub fn parse(value: &str) -> Self {
        let raw = unquote(value).to_owned();
        let characters: Vec<char> = raw.chars().collect();
        let mut segments = Vec::new();
        let mut index = 0usize;

        while index < characters.len() {
            let character = characters[index];
            match character {
                '\\' => {
                    // `\xHH` is an escape; `\$` is an escaped `$` that still
                    // means a substitution (Speeduino writes `\$tsCanId`).
                    match characters.get(index + 1) {
                        Some('x' | 'X') => {
                            let high = characters.get(index + 2).and_then(|c| c.to_digit(16));
                            let low = characters.get(index + 3).and_then(|c| c.to_digit(16));
                            if let (Some(high), Some(low)) = (high, low) {
                                segments.push(Segment::Byte(hex_byte(high, low)));
                                index += 4;
                            } else {
                                segments.push(Segment::Unparsed("\\x".to_owned()));
                                index += 2;
                            }
                        }
                        Some('$') => {
                            index += 1;
                        }
                        Some(other) => {
                            segments.push(Segment::Byte(command_byte(*other)));
                            index += 2;
                        }
                        None => {
                            segments.push(Segment::Byte(b'\\'));
                            index += 1;
                        }
                    }
                }
                '$' => {
                    let start = index + 1;
                    let mut end = start;
                    while end < characters.len()
                        && (characters[end].is_alphanumeric() || characters[end] == '_')
                    {
                        end += 1;
                    }
                    if end == start {
                        segments.push(Segment::Unparsed("$".to_owned()));
                        index += 1;
                    } else {
                        segments.push(Segment::Variable(characters[start..end].iter().collect()));
                        index = end;
                    }
                }
                '%' => {
                    let rest: String = characters[index..].iter().take(3).collect();
                    let lowered = rest.to_ascii_lowercase();
                    let placeholder = match lowered.as_str() {
                        "%2i" => Some((Placeholder::PageId, 3)),
                        "%2o" => Some((Placeholder::Offset, 3)),
                        "%2c" => Some((Placeholder::Count, 3)),
                        "%2v" => Some((Placeholder::Values, 3)),
                        _ if lowered.starts_with("%v") => Some((Placeholder::Value, 2)),
                        _ => None,
                    };
                    if let Some((placeholder, length)) = placeholder {
                        segments.push(Segment::Placeholder(placeholder));
                        index += length;
                    } else {
                        segments.push(Segment::Unparsed("%".to_owned()));
                        index += 1;
                    }
                }
                other if other.is_ascii() => {
                    segments.push(Segment::Byte(command_byte(other)));
                    index += 1;
                }
                other => {
                    segments.push(Segment::Unparsed(other.to_string()));
                    index += 1;
                }
            }
        }

        Self { raw, segments }
    }

    /// Whether the command is absent or an empty string (a page that cannot be
    /// burned or written).
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Bytes for this command, with substitutions applied.
    ///
    /// `variables` resolves a `$name` to one byte; `None` means the variable is
    /// unknown, which is an error rather than a silent zero.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError`] when a variable is unknown, a `%v` placeholder
    /// has no payload, or the command contains text this build cannot encode.
    pub fn encode(
        &self,
        arguments: &CommandArguments<'_>,
        variables: &dyn Fn(&str) -> Option<u8>,
    ) -> Result<Vec<u8>, EncodeError> {
        let mut bytes = Vec::new();
        for segment in &self.segments {
            match segment {
                Segment::Byte(byte) => bytes.push(*byte),
                Segment::Variable(name) => match variables(name) {
                    Some(value) => bytes.push(value),
                    None => return Err(format!("unknown command variable ${name}").into()),
                },
                Segment::Placeholder(Placeholder::PageId) => {
                    // The page identifier is the `[canId][page]` pair — read
                    // byte-wise by the firmware, so always big-endian.
                    bytes.extend_from_slice(&arguments.page.to_be_bytes());
                }
                Segment::Placeholder(Placeholder::Offset) => {
                    bytes.extend_from_slice(&word_bytes(
                        arguments.offset,
                        arguments.force_big_endian,
                    ));
                }
                Segment::Placeholder(Placeholder::Count) => {
                    bytes.extend_from_slice(&word_bytes(
                        arguments.count,
                        arguments.force_big_endian,
                    ));
                }
                Segment::Placeholder(Placeholder::Values) => {
                    bytes.extend_from_slice(arguments.values);
                }
                Segment::Placeholder(Placeholder::Value) => match arguments.values.first() {
                    Some(_) => bytes.extend_from_slice(arguments.values),
                    None => {
                        return Err("the command needs at least one payload byte"
                            .to_owned()
                            .into());
                    }
                },
                Segment::Unparsed(text) => {
                    return Err(format!("the command contains unsupported text {text:?}").into());
                }
            }
        }
        Ok(bytes)
    }
}

/// Values substituted into a command template at send time.
#[derive(Debug, Clone, Copy, Default)]
pub struct CommandArguments<'a> {
    /// Page identifier, written as `[canId][page]` for `%2i`.
    pub page: u16,
    /// Start offset for `%2o`. Little-endian on the wire unless
    /// [`Self::force_big_endian`] is set.
    pub offset: u16,
    /// Byte count for `%2c`. Same wire order as [`Self::offset`].
    pub count: u16,
    /// Payload for `%2v` / `%v`.
    pub values: &'a [u8],
    /// The definition's `forceBigEndianProtocol`: offsets and lengths go on
    /// the wire big-endian instead of the dialect's little-endian default.
    /// Callers pass `protocol.force_big_endian_protocol`.
    pub force_big_endian: bool,
}

/// One 16-bit command argument on the wire: little-endian unless the
/// definition forces the big-endian protocol.
fn word_bytes(value: u16, force_big_endian: bool) -> [u8; 2] {
    if force_big_endian {
        value.to_be_bytes()
    } else {
        value.to_le_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_semicolon_in_a_quoted_string_is_not_a_comment() {
        assert_eq!(
            strip_comment(r#"versionInfo = "S" ; shown"#),
            r#"versionInfo = "S" "#
        );
        assert_eq!(strip_comment(r#"a = "x;y""#), r#"a = "x;y""#);
    }

    #[test]
    fn commas_inside_braces_do_not_split_fields() {
        let fields = split_fields(r#"bits, U08, 1, [0:2], { bitStringValue(units, algo) }, "x""#);

        assert_eq!(fields.len(), 6);
        assert_eq!(fields[4], "{ bitStringValue(units, algo) }");
    }

    #[test]
    fn commas_inside_quotes_do_not_split_fields() {
        let fields = split_fields(r#"a, "one, two", b"#);

        assert_eq!(fields, vec!["a", "\"one, two\"", "b"]);
    }

    #[test]
    fn an_assignment_is_split_only_outside_quotes_and_braces() {
        assert_eq!(split_assignment("key = a = b"), Some(("key", "a = b")));
        assert_eq!(
            split_assignment(r#"key = "a=b""#),
            Some(("key", r#""a=b""#))
        );
        assert_eq!(split_assignment("no assignment here"), None);
    }

    #[test]
    fn hex_integers_are_accepted() {
        assert_eq!(parse_u32("0x1F"), Some(31));
        assert_eq!(parse_u32("42"), Some(42));
        assert_eq!(parse_f64("0x10"), Some(16.0));
    }

    #[test]
    fn a_megasquirt_command_expands_to_the_documented_bytes() {
        // The spec's worked example: `"r$tsCanId\x0e\x00\x00\x00\x3c"` with
        // tsCanId = 0 goes on the wire as 72 00 0E 00 00 00 3C.
        let command = CommandTemplate::parse(r#""r$tsCanId\x0e\x00\x00\x00\x3c""#);

        let bytes = command
            .encode(&CommandArguments::default(), &|name| {
                (name == "tsCanId").then_some(0)
            })
            .expect("encodes");

        assert_eq!(bytes, vec![0x72, 0x00, 0x0E, 0x00, 0x00, 0x00, 0x3C]);
    }

    #[test]
    fn placeholders_carry_page_offset_count_and_payload() {
        // Offsets and counts are little-endian on the wire (the dialect
        // default; `forceBigEndianProtocol` flips them).
        // The page identifier is the [canId][page] pair, big-endian.
        let command = CommandTemplate::parse(r#""p%2i%2o%2c""#);

        let bytes = command
            .encode(
                &CommandArguments {
                    page: 0x0102,
                    offset: 0x0004,
                    count: 0x0003,
                    values: &[],
                    force_big_endian: false,
                },
                &|_| None,
            )
            .expect("encodes");

        assert_eq!(bytes, vec![0x70, 0x01, 0x02, 0x04, 0x00, 0x03, 0x00]);
    }

    #[test]
    fn force_big_endian_protocol_flips_offset_and_count() {
        let command = CommandTemplate::parse(r#""p%2i%2o%2c""#);

        let bytes = command
            .encode(
                &CommandArguments {
                    page: 1,
                    offset: 4,
                    count: 3,
                    force_big_endian: true,
                    ..CommandArguments::default()
                },
                &|_| None,
            )
            .expect("encodes");

        assert_eq!(bytes, vec![0x70, 0x00, 0x01, 0x00, 0x04, 0x00, 0x03]);
    }

    #[test]
    fn an_escaped_dollar_still_substitutes() {
        // Speeduino writes `"\$tsCanId\x01"`; the backslash only escapes the
        // dollar for the INI reader.
        let command = CommandTemplate::parse(r#""\$tsCanId\x01""#);

        let bytes = command
            .encode(&CommandArguments::default(), &|_| Some(0x07))
            .expect("encodes");

        assert_eq!(bytes, vec![0x07, 0x01]);
    }

    #[test]
    fn an_unknown_variable_is_an_error_not_a_zero_byte() {
        let command = CommandTemplate::parse(r#""$missing""#);

        let error = command
            .encode(&CommandArguments::default(), &|_| None)
            .expect_err("must not invent a byte");

        assert!(error.to_string().contains("$missing"));
    }

    #[test]
    fn a_payload_placeholder_accepts_a_whole_block() {
        let command = CommandTemplate::parse(r#""M%2i%2o%2c%2v""#);

        let bytes = command
            .encode(
                &CommandArguments {
                    page: 1,
                    offset: 2,
                    count: 3,
                    values: &[9, 8, 7],
                    force_big_endian: false,
                },
                &|_| None,
            )
            .expect("encodes");

        assert_eq!(bytes, vec![0x4D, 0, 1, 2, 0, 3, 0, 9, 8, 7]);
    }

    #[test]
    fn a_single_value_placeholder_writes_every_payload_byte() {
        // The corpus uses `%v` for a word as well as for one byte: the count
        // says how many bytes follow, and the firmware reads that many.
        let command = CommandTemplate::parse(r#""M%2i%2o%2c%v""#);

        let bytes = command
            .encode(
                &CommandArguments {
                    page: 1,
                    offset: 0,
                    count: 2,
                    values: &[0xAB, 0xCD],
                    force_big_endian: false,
                },
                &|_| None,
            )
            .expect("encodes");

        assert_eq!(bytes, vec![0x4D, 0, 1, 0, 0, 2, 0, 0xAB, 0xCD]);
    }
}
