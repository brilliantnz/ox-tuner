//! The frozen definition model: what the rest of ox-tuner consumes.
//!
//! Everything here is plain data with no GPUI, no I/O and no ECU knowledge
//! (ADR-0001, ADR-0006). The parser builds it; nothing mutates it afterwards.
//!
//! Order is preserved wherever the dialect makes it meaningful — `[Constants]`
//! load order drives calibration-file field order, so a `HashMap` alone would
//! lose information that later crates need.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::fmt::Write as _;
use std::sync::Arc;

use crate::error::Diagnostic;
use crate::expr::Expr;
use crate::fields::Command;

/// A name-keyed, insertion-ordered collection.
///
/// `entries` is the source of truth for order and contents. The two maps are
/// indexes into it: `lookup` answers an exact name, `ci_lookup` answers the
/// trimmed ASCII-lowercase form `get_ci` falls back to. Both are rebuilt by
/// `insert` and nothing else renames or removes an entry, so they cannot drift.
#[derive(Debug, Clone, PartialEq)]
pub struct Ordered<T> {
    entries: Vec<(String, T)>,
    lookup: HashMap<String, usize>,
    /// Hash of the folded name -> first entry with that hash.
    ///
    /// A hash rather than a lowercased `String` key: building the index must
    /// not allocate once per entry, because a large definition inserts
    /// hundreds of constants during a parse. A hash collision keeps the first
    /// index and makes `get_ci` fall back to the linear scan, which preserves
    /// the first-declaration rule without ever changing a result.
    ci_lookup: HashMap<u64, usize>,
}

/// FNV-1a over the ASCII-lowercased bytes.
///
/// `eq_ignore_ascii_case` folds only A–Z, and so does this; non-ASCII bytes
/// are hashed unchanged. The name is hashed untrimmed at insertion and the
/// query trimmed at lookup, which is exactly the comparison `get_ci` makes.
fn ci_hash(name: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &byte in name.as_bytes() {
        hash ^= u64::from(byte.to_ascii_lowercase());
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

impl<T> Default for Ordered<T> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            lookup: HashMap::new(),
            ci_lookup: HashMap::new(),
        }
    }
}

impl<T> Ordered<T> {
    /// An empty collection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts an entry, replacing an existing one **in place** so definition
    /// order survives a redefinition.
    pub fn insert(&mut self, name: impl Into<String>, value: T) -> Option<T> {
        let name = name.into();
        if let Some(&position) = self.lookup.get(&name) {
            let previous = std::mem::replace(&mut self.entries[position].1, value);
            return Some(previous);
        }

        let position = self.entries.len();
        // The case-insensitive index keeps the first declaration: two names
        // that differ only by case are distinct entries, and a `get_ci` has
        // always returned the earlier one.
        self.ci_lookup.entry(ci_hash(&name)).or_insert(position);
        self.lookup.insert(name.clone(), position);
        self.entries.push((name, value));
        None
    }

    /// Looks an entry up by exact name.
    pub fn get(&self, name: &str) -> Option<&T> {
        self.lookup
            .get(name)
            .map(|position| &self.entries[*position].1)
    }

    /// Looks an entry up case-insensitively, the way references in the dialect
    /// are usually written.
    ///
    /// The exact index is tried first without trimming, then the hash of the
    /// trimmed ASCII-folded form. That is the semantics the previous linear
    /// scan had: candidates were never trimmed, so a name with surrounding
    /// spaces only matched an equally spaced query.
    pub fn get_ci(&self, name: &str) -> Option<&T> {
        if let Some(found) = self.get(name) {
            return Some(found);
        }

        let name = name.trim();
        match self.ci_lookup.get(&ci_hash(name)) {
            Some(&position) => {
                let (candidate, value) = &self.entries[position];
                if candidate.eq_ignore_ascii_case(name) {
                    return Some(value);
                }

                // A hash collision stored another name first. The linear scan
                // is the oracle for that case; it is unreachable in practice
                // with a 64-bit hash, but keeping it makes a collision a
                // slowdown rather than a wrong answer.
                self.entries
                    .iter()
                    .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
                    .map(|(_, value)| value)
            }
            None => None,
        }
    }

    /// Mutably borrows an entry by exact name.
    pub fn get_mut(&mut self, name: &str) -> Option<&mut T> {
        let position = *self.lookup.get(name)?;
        Some(&mut self.entries[position].1)
    }

    /// Whether the name is present (exact or case-insensitive).
    pub fn contains(&self, name: &str) -> bool {
        self.get_ci(name).is_some()
    }

    /// Entries in definition order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &T)> {
        self.entries
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }

    /// Values only, in definition order.
    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.entries.iter().map(|(_, value)| value)
    }

    /// Values mutably, in definition order.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&str, &mut T)> {
        self.entries
            .iter_mut()
            .map(|(name, value)| (name.as_str(), value))
    }

    /// Names in definition order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(name, _)| name.as_str())
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the collection is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Serializes only the ordered entries; the indexes are an implementation
/// detail and are rebuilt on load.
impl<T: serde::Serialize> serde::Serialize for Ordered<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_seq(&self.entries)
    }
}

impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for Ordered<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let entries = <Vec<(String, T)> as serde::Deserialize<'de>>::deserialize(deserializer)?;
        let mut ordered = Self::new();
        for (name, value) in entries {
            ordered.insert(name, value);
        }
        Ok(ordered)
    }
}

/// Byte order of multi-byte values.
#[expect(
    clippy::exhaustive_enums,
    reason = "a number is either little- or big-endian; the set cannot grow"
)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Endianness {
    /// Low byte first. The dialect's documented default is big, but every
    /// Speeduino and rusEFI file states `little` explicitly.
    Little,
    /// High byte first.
    #[default]
    Big,
}

impl Endianness {
    /// Parses `little` / `big`.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "little" | "lsb" | "intel" => Some(Self::Little),
            "big" | "msb" | "motorola" => Some(Self::Big),
            _ => None,
        }
    }
}

impl fmt::Display for Endianness {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Little => "little",
            Self::Big => "big",
        })
    }
}

/// The numeric and string types a constant or channel can use.
#[expect(
    clippy::exhaustive_enums,
    reason = "the INI dialect defines a closed set of wire data types"
)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataType {
    /// Unsigned 8-bit.
    U08,
    /// Signed 8-bit.
    S08,
    /// Unsigned 16-bit.
    U16,
    /// Signed 16-bit.
    S16,
    /// Unsigned 32-bit.
    U32,
    /// Signed 32-bit.
    S32,
    /// Unsigned 64-bit.
    U64,
    /// Signed 64-bit.
    S64,
    /// IEEE-754 single precision.
    F32,
    /// Fixed-length ASCII text.
    Ascii,
    /// Unsigned until its maximum, then wraps negative (iniSpec 3.43).
    M08,
}

impl DataType {
    /// Parses the dialect's type tokens.
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value.trim().to_ascii_uppercase().as_str() {
            "U08" | "UBYTE" => Self::U08,
            "S08" | "SBYTE" => Self::S08,
            "U16" => Self::U16,
            "S16" => Self::S16,
            "U32" => Self::U32,
            "S32" => Self::S32,
            "U64" => Self::U64,
            "S64" => Self::S64,
            "F32" | "FLOAT" => Self::F32,
            "ASCII" => Self::Ascii,
            "M08" => Self::M08,
            _ => return None,
        })
    }

    /// Width in bytes. `Ascii` is one byte per character, so callers must use
    /// the declared length rather than this value.
    pub fn size(self) -> u32 {
        match self {
            Self::U08 | Self::S08 | Self::M08 | Self::Ascii => 1,
            Self::U16 | Self::S16 => 2,
            Self::U32 | Self::S32 | Self::F32 => 4,
            Self::U64 | Self::S64 => 8,
        }
    }

    /// Whether the type stores text.
    pub fn is_text(self) -> bool {
        matches!(self, Self::Ascii)
    }
}

impl fmt::Display for DataType {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::U08 => "U08",
            Self::S08 => "S08",
            Self::U16 => "U16",
            Self::S16 => "S16",
            Self::U32 => "U32",
            Self::S32 => "S32",
            Self::U64 => "U64",
            Self::S64 => "S64",
            Self::F32 => "F32",
            Self::Ascii => "ASCII",
            Self::M08 => "M08",
        })
    }
}

/// A numeric attribute: a literal, or an expression that stays dynamic.
#[expect(
    clippy::exhaustive_enums,
    reason = "a numeric attribute is either a literal or an expression"
)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub enum Number {
    /// A value known at parse time.
    Literal(f64),
    /// An expression, kept as an AST because presentation attributes
    /// re-evaluate at runtime.
    Expression {
        /// The expression as written.
        source: String,
        /// `None` when the expression could not be parsed.
        parsed: Option<Expr>,
    },
}

impl Number {
    /// Parses a numeric field, with or without braces.
    pub fn parse(field: &str) -> Self {
        let field = field.trim();
        if crate::fields::is_expression_field(field) {
            let source = crate::fields::expression_body(field)
                .unwrap_or("")
                .to_owned();
            let parsed = Expr::parse(&source).ok();
            return Self::Expression { source, parsed };
        }
        match crate::fields::parse_f64(field) {
            Some(value) => Self::Literal(value),
            None => Self::Expression {
                source: field.to_owned(),
                parsed: Expr::parse(field).ok(),
            },
        }
    }

    /// The literal value, when there is one.
    pub fn literal(&self) -> Option<f64> {
        match self {
            Self::Literal(value) => Some(*value),
            Self::Expression { .. } => None,
        }
    }

    /// Folds the value when it depends only on known names.
    pub fn evaluate(&self, lookup: &dyn Fn(&str) -> Option<f64>) -> Option<f64> {
        match self {
            Self::Literal(value) => Some(*value),
            Self::Expression { parsed, .. } => parsed.as_ref()?.evaluate(lookup),
        }
    }

    /// Whether this attribute has to be recomputed at runtime.
    pub fn is_dynamic(&self) -> bool {
        matches!(self, Self::Expression { .. })
    }
}

/// A whole number in the range the display form treats as an integer.
#[expect(
    clippy::cast_possible_truncation,
    clippy::as_conversions,
    reason = "the caller checks fract() == 0 and abs < 1e15 before converting"
)]
fn whole_number(value: f64) -> i64 {
    value as i64
}

impl fmt::Display for Number {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Literal(value) => {
                if value.fract() == 0.0 && value.abs() < 1e15 {
                    write!(formatter, "{}", whole_number(*value))
                } else {
                    write!(formatter, "{value}")
                }
            }
            Self::Expression { source, .. } => write!(formatter, "{{{source}}}"),
        }
    }
}

/// A text attribute: a literal string, or a string function such as
/// `{ bitStringValue(algorithmUnits, algorithm) }`.
#[expect(
    clippy::exhaustive_enums,
    reason = "a text attribute is either a literal or an expression"
)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub enum Text {
    /// Plain text, quotes removed.
    Literal(String),
    /// A `{ … }` expression that produces text at runtime.
    Expression {
        /// The expression as written.
        source: String,
        /// `None` when it could not be parsed.
        parsed: Option<Expr>,
    },
}

impl Text {
    /// Parses a units/label field.
    pub fn parse(field: &str) -> Self {
        let field = field.trim();
        if crate::fields::is_expression_field(field) {
            let source = crate::fields::expression_body(field)
                .unwrap_or("")
                .to_owned();
            let parsed = Expr::parse(&source).ok();
            return Self::Expression { source, parsed };
        }
        Self::Literal(crate::fields::unquote(field).to_owned())
    }

    /// The literal text, when there is one.
    pub fn literal(&self) -> Option<&str> {
        match self {
            Self::Literal(text) => Some(text),
            Self::Expression { .. } => None,
        }
    }

    /// Whether the text has to be recomputed at runtime.
    pub fn is_dynamic(&self) -> bool {
        matches!(self, Self::Expression { .. })
    }
}

impl fmt::Display for Text {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Literal(text) => formatter.write_str(text),
            Self::Expression { source, .. } => write!(formatter, "{{{source}}}"),
        }
    }
}

/// A dimension: fixed, or an expression over constants (resizable arrays).
#[expect(
    clippy::exhaustive_enums,
    reason = "a dimension is either fixed or an expression; the set cannot grow"
)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub enum Dimension {
    /// Known at parse time.
    Fixed(u32),
    /// Resolved at runtime from constant values (iniSpec 3.12/3.22/3.53).
    Expression(Expr),
}

/// A non-negative dimension expression converted to the model's element count.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::as_conversions,
    reason = "the caller checks the value is non-negative; dimensions are element counts"
)]
fn dimension_value(value: f64) -> u32 {
    value as u32
}

impl Dimension {
    /// The value, when it is fixed.
    pub fn value(&self) -> Option<u32> {
        match self {
            Self::Fixed(value) => Some(*value),
            Self::Expression(_) => None,
        }
    }

    /// Folds the value with `lookup` supplying names.
    pub fn evaluate(&self, lookup: &dyn Fn(&str) -> Option<f64>) -> Option<u32> {
        match self {
            Self::Fixed(value) => Some(*value),
            Self::Expression(expression) => expression
                .evaluate(lookup)
                .filter(|value| *value >= 0.0)
                .map(dimension_value),
        }
    }
}

impl fmt::Display for Dimension {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fixed(value) => write!(formatter, "{value}"),
            // The AST has no source-preserving printer; the parser keeps the
            // written form on the owning entry, so this is only ever a label.
            Self::Expression(_) => formatter.write_str("{...}"),
        }
    }
}

/// The declared size of a constant.
#[expect(
    clippy::exhaustive_enums,
    reason = "the INI dialect defines a closed set of storage shapes"
)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub enum Shape {
    /// One value.
    Scalar,
    /// A bit field inside a larger word: `[low:high]`, with the optional
    /// `[0:2+1]` offset that makes the drop-down start at 1.
    Bits {
        /// Lowest bit, inclusive.
        low: u8,
        /// Highest bit, inclusive.
        high: u8,
        /// Added to the presented value (`+1` in `[0:2+1]`).
        shift: u8,
    },
    /// A one-dimensional array: `[n]`.
    List(Dimension),
    /// A two-dimensional array: `[cols x rows]` — **columns first**.
    Table {
        /// Number of columns.
        cols: Dimension,
        /// Number of rows.
        rows: Dimension,
    },
    /// Fixed-length text: `string, ASCII, length[, columns]`.
    Text {
        /// Number of bytes.
        length: Dimension,
        /// Optional display width.
        columns: Option<Dimension>,
    },
}

impl Shape {
    /// Number of elements, when the dimensions are fixed.
    pub fn element_count(&self) -> Option<u32> {
        match self {
            Self::Scalar => Some(1),
            Self::Bits { low, high, .. } => {
                let width = high.saturating_sub(*low).saturating_add(1);
                Some(1u32.checked_shl(u32::from(width)).unwrap_or(u32::MAX))
            }
            Self::List(length) | Self::Text { length, .. } => length.value(),
            Self::Table { cols, rows } => Some(cols.value()? * rows.value()?),
        }
    }

    /// Whether any dimension still needs runtime resolution.
    pub fn is_dynamic(&self) -> bool {
        match self {
            Self::Scalar | Self::Bits { .. } => false,
            Self::List(length) | Self::Text { length, .. } => length.value().is_none(),
            Self::Table { cols, rows } => cols.value().is_none() || rows.value().is_none(),
        }
    }
}

impl fmt::Display for Shape {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Scalar => formatter.write_str("scalar"),
            Self::Bits { low, high, shift } => {
                if *shift == 0 {
                    write!(formatter, "[{low}:{high}]")
                } else {
                    write!(formatter, "[{low}:{high}+{shift}]")
                }
            }
            Self::List(length) => write!(formatter, "[{length}]"),
            Self::Table { cols, rows } => write!(formatter, "[{cols}x{rows}]"),
            Self::Text { length, columns } => match columns {
                Some(columns) => write!(formatter, "string[{length}, {columns}]"),
                None => write!(formatter, "string[{length}]"),
            },
        }
    }
}

/// What kind of entry a constant is.
#[expect(
    clippy::exhaustive_enums,
    reason = "the INI dialect defines a closed set of constant classes"
)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstantClass {
    /// A bit field with an option list.
    Bits,
    /// One value.
    Scalar,
    /// One- or two-dimensional array.
    Array,
    /// Fixed-length text.
    String,
    /// An array shown every other row (read-only half-size view).
    OddArray,
}

impl ConstantClass {
    /// Parses the class token.
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value.trim().to_ascii_lowercase().as_str() {
            "bits" => Self::Bits,
            "scalar" => Self::Scalar,
            "array" => Self::Array,
            "string" => Self::String,
            "oddarray" => Self::OddArray,
            _ => return None,
        })
    }
}

/// One label in a `bits` option list.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct BitOption {
    /// The value this label stands for: the explicit `3="Three"` number, or
    /// the label's position when the dialect leaves it out.
    pub value: u32,
    /// The text shown in the drop-down. `INVALID` entries are hidden by the UI.
    pub label: String,
    /// Whether the dialect marks this option as hidden (`"INVALID"`).
    pub hidden: bool,
}

/// Storage modifiers that change who owns a constant's value.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ConstantFlags {
    /// Never loaded from calibration files.
    pub no_msq_save: bool,
    /// Saved offline, but silently overwritten from the ECU on connect.
    pub controller_priority: bool,
}

/// How the offset was written.
#[expect(
    clippy::exhaustive_enums,
    reason = "the INI dialect defines a closed set of offset forms"
)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub enum Offset {
    /// An explicit address (decimal or hex).
    Fixed(u32),
    /// `nextOffset`: directly after the previous constant.
    Next,
    /// `lastOffset`: the same address as the previous constant.
    Last,
    /// An expression, resolved at load time when its inputs are known.
    Expression(Expr),
    /// The offset could not be interpreted; the parser reported a
    /// `bad-offset` diagnostic. This is **not** an address: consumers must
    /// treat the constant's memory location as unknown, never as byte 0.
    Unknown,
}

/// One editable calibration parameter.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Constant {
    /// Name used by expressions and by the `.msq`.
    pub name: String,
    /// Entry class.
    pub class: ConstantClass,
    /// Element type.
    pub data_type: DataType,
    /// Page number as written, 1-based.
    pub page: u8,
    /// Offset as written.
    pub offset: Offset,
    /// Address inside the page, when the definition layer could resolve it.
    pub address: Option<u32>,
    /// Declared shape.
    pub shape: Shape,
    /// Display units.
    pub units: Text,
    /// User-value scale (`user = (raw + translate) × scale`).
    pub scale: Option<Number>,
    /// User-value translate.
    pub translate: Option<Number>,
    /// Minimum user value.
    pub minimum: Option<Number>,
    /// Maximum user value.
    pub maximum: Option<Number>,
    /// Decimal digits to display.
    pub digits: Option<Number>,
    /// Option list for `bits`, shared between entries that declare the same
    /// `$define` (Speeduino repeats a 65 536-label CAN address list twenty
    /// times).
    pub options: Arc<[BitOption]>,
    /// Storage modifiers written on the entry itself.
    pub flags: ConstantFlags,
    /// Source line.
    pub line: u32,
    /// The entry as written, for round-tripping and diagnostics.
    pub raw: String,
}

impl Constant {
    /// Size in bytes, when the shape is fully known.
    pub fn byte_size(&self) -> Option<u32> {
        match self.class {
            ConstantClass::Bits => Some(self.data_type.size()),
            ConstantClass::String => match &self.shape {
                Shape::Text { length, .. } => length.value(),
                _ => None,
            },
            _ => self
                .shape
                .element_count()
                .and_then(|count| count.checked_mul(self.data_type.size())),
        }
    }
}

/// A PC-side variable: same syntax as a constant, stored only in the project.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct PcVariable {
    /// Name used by expressions and `$name` command substitutions.
    pub name: String,
    /// Entry class.
    pub class: ConstantClass,
    /// Element type.
    pub data_type: DataType,
    /// Declared shape.
    pub shape: Shape,
    /// Display units.
    pub units: Text,
    /// Option list for `bits` (the value of a bit PC variable is its index).
    pub options: Arc<[BitOption]>,
    /// Whether the value is a snapshot taken at connect time.
    pub channel_value_on_connect: Option<String>,
    /// Whether the value tracks a live channel.
    pub continuous_channel_value: Option<String>,
    /// Source line.
    pub line: u32,
    /// The entry as written.
    pub raw: String,
}

/// A bit range inside a channel's backing word.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct BitRange {
    /// Lowest bit, inclusive.
    pub low: u8,
    /// Highest bit, inclusive.
    pub high: u8,
}

impl BitRange {
    /// Number of bits the field spans.
    pub fn width(self) -> u8 {
        self.high.saturating_sub(self.low).saturating_add(1)
    }
}

/// How a realtime channel gets its value.
#[expect(
    clippy::exhaustive_enums,
    reason = "a channel is either measured from the realtime block or computed"
)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub enum ChannelKind {
    /// Read from the realtime block at an offset.
    Measured {
        /// Wire type of the value.
        data_type: DataType,
        /// Type of the word a bit field is cut from (equals `data_type` for
        /// non-bit channels).
        storage: DataType,
        /// Byte offset in the realtime block.
        offset: u32,
        /// Bit range, for `bits` channels.
        bits: Option<BitRange>,
    },
    /// Computed from other channels and constants.
    Computed(Expr),
}

/// One realtime value.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Channel {
    /// Channel name.
    pub name: String,
    /// Where the value comes from.
    pub kind: ChannelKind,
    /// Display units.
    pub units: Text,
    /// Scale, for measured channels.
    pub scale: Number,
    /// Translate, for measured channels.
    pub translate: Number,
    /// Whether the definition hides the channel from pickers.
    pub hidden: bool,
    /// Source line.
    pub line: u32,
    /// The entry as written.
    pub raw: String,
}

impl Channel {
    /// Whether the value is computed rather than read.
    pub fn is_computed(&self) -> bool {
        matches!(self.kind, ChannelKind::Computed(_))
    }
}

/// Scattered ("full optimized") realtime reads.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct ScatteredConfig {
    /// Command used for a scattered read.
    pub get_command: Option<Command>,
    /// Name of the U16 offset array constant.
    pub offset_array: Option<String>,
    /// Expression that says whether scattered reads are enabled.
    pub enabled: Option<String>,
}

/// Communication parameters, derived **after** settings resolution.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Protocol {
    /// Maximum payload bytes per read/write chunk.
    pub blocking_factor: Option<u32>,
    /// Delay between written bytes, in milliseconds.
    pub inter_write_delay_ms: Option<u32>,
    /// Whether writes ignore `inter_write_delay`.
    pub write_blocks: bool,
    /// Maximum wait for a response block, in milliseconds.
    pub block_read_timeout_ms: Option<u32>,
    /// Wait after a page switch, in milliseconds.
    pub page_activation_delay_ms: Option<u32>,
    /// Wait after opening the port, in milliseconds.
    pub delay_after_port_open_ms: Option<u32>,
    /// Legacy reads have no delay.
    pub no_comm_read_delay: bool,
    /// Framing envelope, e.g. `msEnvelope_1.0`.
    pub envelope: Option<String>,
    /// Offsets and lengths are big-endian on the wire even when data is little.
    pub force_big_endian_protocol: bool,
    /// Command that reads the realtime block.
    pub och_get_command: Option<Command>,
    /// Size of the realtime block, in bytes.
    pub och_block_size: Option<u32>,
    /// Base added to realtime offsets.
    pub output_channel_start_offset: u32,
    /// Scattered-read configuration, when the definition provides one.
    pub scattered: Option<ScatteredConfig>,
    /// Default realtime sample rate.
    pub default_runtime_record_per_sec: Option<u32>,
    /// Channel-count hint for block sizing; 0 disables it.
    pub max_unused_runtime_range: u32,
    /// Command that retrieves the configuration error text.
    pub retrieve_config_error: Option<Command>,
    /// Command that starts a burst read (`[BurstMode]`).
    pub burst_get_command: Option<Command>,
    /// Every recognised protocol key with its raw value, so nothing is lost
    /// before the protocol crate models it.
    pub raw: BTreeMap<String, String>,
}

/// One calibration page.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Page {
    /// Page number as written, 1-based.
    pub number: u8,
    /// Page size in bytes.
    pub size: u32,
    /// Base address added to every constant offset on this page.
    pub start_offset: u32,
    /// Two-byte page identifier used by `%2i`.
    pub identifier: Option<Command>,
    /// Command that reads part of this page.
    pub read_command: Option<Command>,
    /// Command that burns this page. `None` (or an empty command) means the
    /// page cannot be burned.
    pub burn_command: Option<Command>,
    /// Command that writes a single value.
    pub value_write: Option<Command>,
    /// Command that writes a chunk.
    pub chunk_write: Option<Command>,
    /// Command that returns the page's CRC32.
    pub crc_command: Option<Command>,
}

impl Page {
    /// Whether the page can be burned at all.
    pub fn is_burnable(&self) -> bool {
        self.burn_command
            .as_ref()
            .is_some_and(|command| !command.is_empty())
    }

    /// Whether the page can be written at all.
    pub fn is_writable(&self) -> bool {
        let usable =
            |command: &Option<Command>| command.as_ref().is_some_and(|command| !command.is_empty());
        usable(&self.value_write) || usable(&self.chunk_write)
    }
}

/// One axis of a table: an array constant plus the live channel that moves the
/// cursor.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Axis {
    /// Array constant holding the axis values.
    pub array: String,
    /// Channel that tracks the current position.
    pub channel: Option<String>,
    /// Whether the axis is read-only in this view.
    pub read_only: bool,
}

/// A 2-D table from `[TableEditor]`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Table {
    /// View id (also names the `.table` snapshot).
    pub id: String,
    /// 3-D dashboard id (also names the `.dash` file).
    pub map_id: Option<String>,
    /// Window title.
    pub title: Text,
    /// Page the cell array lives on, when declared.
    pub page: Option<u8>,
    /// X axis.
    pub x_bins: Option<Axis>,
    /// Y axis.
    pub y_bins: Option<Axis>,
    /// Cell array.
    pub z_bins: Option<String>,
    /// Axis label overrides.
    pub xy_labels: Vec<Text>,
    /// Wording for the 3-D vertical axis.
    pub up_down_label: Vec<Text>,
    /// 3-D camera height.
    pub grid_height: Option<Number>,
    /// 3-D camera angles (yaw, pitch, roll).
    pub grid_orient: Option<[Number; 3]>,
    /// Help topic.
    pub topic_help: Option<String>,
    /// Source line of the `table =` entry.
    pub line: u32,
}

/// A 1-D curve from `[CurveEditor]`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Curve {
    /// Curve name.
    pub name: String,
    /// Window title.
    pub title: Text,
    /// Column labels.
    pub column_labels: Vec<Text>,
    /// X axis range (min, max, divisions).
    pub x_axis: Vec<Number>,
    /// Y axis range (min, max, divisions).
    pub y_axis: Vec<Number>,
    /// X bins, with optional tracking channel.
    pub x_bins: Option<Axis>,
    /// One entry per plotted curve.
    pub y_bins: Vec<Axis>,
    /// Per-curve line labels.
    pub line_labels: Vec<Text>,
    /// Optional gauge to draw.
    pub gauge: Option<String>,
    /// Whether text values are shown next to the graph.
    pub show_text_values: bool,
    /// Whether the graph is suppressed (table-only display).
    pub suppress_graph: bool,
    /// Source line.
    pub line: u32,
}

/// One gauge template from `[GaugeConfigurations]`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Gauge {
    /// Category the template belongs to.
    pub category: Option<String>,
    /// Template name.
    pub name: String,
    /// Channel the gauge shows.
    pub channel: String,
    /// Display title.
    pub title: Text,
    /// Display units.
    pub units: Text,
    /// Lowest value.
    pub low: Number,
    /// Highest value.
    pub high: Number,
    /// Low danger threshold.
    pub low_danger: Number,
    /// Low warning threshold.
    pub low_warning: Number,
    /// High warning threshold.
    pub high_warning: Number,
    /// High danger threshold.
    pub high_danger: Number,
    /// Value digits.
    pub value_digits: Number,
    /// Label digits.
    pub label_digits: Number,
    /// Optional availability expression.
    pub active: Option<Expr>,
    /// Source line.
    pub line: u32,
    /// Availability expression source, as written.
    ///
    /// `Expr` keeps no source text, so the parser stores it beside the parsed
    /// form for summaries that carry gates as data.
    pub active_source: Option<String>,
}

/// One dashboard indicator.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Indicator {
    /// Channel (or expression source) that drives it.
    pub channel: String,
    /// Text when off.
    pub off_label: String,
    /// Text when on.
    pub on_label: String,
    /// Remaining colour fields, as written.
    pub colours: Vec<String>,
    /// Whether this is a template-only entry.
    pub template_only: bool,
    /// Source line.
    pub line: u32,
}

/// `[FrontPage]`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct FrontPage {
    /// Gauges in slot order.
    pub gauges: Vec<(u8, String)>,
    /// Indicators.
    pub indicators: Vec<Indicator>,
}

impl FrontPage {
    /// Gauges in slot order.
    pub fn slots(&self) -> &[(u8, String)] {
        &self.gauges
    }
}

/// The row kinds the dynamic dialog system defines.
#[expect(
    clippy::exhaustive_enums,
    reason = "the dialog grammar has a fixed set of row kinds; unknown text maps to Unknown"
)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// `field` — label plus constant.
    Field,
    /// `displayOnlyField`.
    DisplayOnlyField,
    /// `checkbox`.
    Checkbox,
    /// `radio`.
    Radio,
    /// `slider`.
    Slider,
    /// `settingSelector`.
    SettingSelector,
    /// `settingOption`.
    SettingOption,
    /// `array1D`.
    Array1D,
    /// `runtimeValue`.
    RuntimeValue,
    /// `panel` — embeds another dialog, table, curve or standard dialog.
    Panel,
    /// `gauge`.
    Gauge,
    /// `liveGraph`.
    LiveGraph,
    /// `graphLine`.
    GraphLine,
    /// `indicatorPanel`.
    IndicatorPanel,
    /// `indicator`.
    Indicator,
    /// `readoutPanel`.
    ReadoutPanel,
    /// `readout`.
    Readout,
    /// `commandButton`.
    CommandButton,
    /// `help` / `text`.
    Help,
    /// `webHelp`.
    WebHelp,
    /// `topicHelp`.
    TopicHelp,
    /// Anything this build does not model yet; kept as data.
    Unknown,
}

impl RowKind {
    /// Maps a dialog row keyword.
    pub fn parse(keyword: &str) -> Self {
        match keyword.trim().to_ascii_lowercase().as_str() {
            "field" => Self::Field,
            "displayonlyfield" => Self::DisplayOnlyField,
            "checkbox" => Self::Checkbox,
            "radio" => Self::Radio,
            "slider" => Self::Slider,
            "settingselector" => Self::SettingSelector,
            "settingoption" => Self::SettingOption,
            "array1d" => Self::Array1D,
            "runtimevalue" => Self::RuntimeValue,
            "panel" => Self::Panel,
            "gauge" => Self::Gauge,
            "livegraph" => Self::LiveGraph,
            "graphline" => Self::GraphLine,
            "indicatorpanel" => Self::IndicatorPanel,
            "indicator" => Self::Indicator,
            "readoutpanel" => Self::ReadoutPanel,
            "readout" => Self::Readout,
            "commandbutton" => Self::CommandButton,
            "help" | "text" => Self::Help,
            "webhelp" => Self::WebHelp,
            "topichelp" => Self::TopicHelp,
            _ => Self::Unknown,
        }
    }
}

/// One row of a dialog.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DialogRow {
    /// The keyword.
    pub kind: RowKind,
    /// The keyword as written.
    pub keyword: String,
    /// Fields after the keyword, brace- and quote-aware.
    pub fields: Vec<String>,
    /// Source line.
    pub line: u32,
}

/// A dialog from `[UiDialogs]` / `[UserDefined]`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Dialog {
    /// Dialog id (a stable key for window geometry).
    pub name: String,
    /// Title, when present.
    pub title: Option<String>,
    /// Layout: `yAxis`, `xAxis`, `border`, `card`, `indexCard`.
    pub layout: Option<String>,
    /// Rows in document order.
    pub rows: Vec<DialogRow>,
    /// Source line of the `dialog =` entry.
    pub line: u32,
}

/// What a menu item points at.
#[expect(
    clippy::exhaustive_enums,
    reason = "the menu grammar has a fixed set of target kinds"
)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuTargetKind {
    /// Another dialog / table / curve.
    Panel,
    /// A divider.
    Separator,
    /// A group header declared with `groupMenu`.
    Group,
    /// One of the application's standard dialogs (`std_*`).
    Standard,
}

/// One menu item.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct MenuItem {
    /// Item kind.
    pub kind: MenuTargetKind,
    /// Target id, empty for a separator and for a group header.
    pub target: String,
    /// Label as written (may contain `&`).
    pub label: String,
    /// Visibility/enable expressions, as written.
    pub conditions: Vec<String>,
    /// Index in the owning menu's `items` of the `groupMenu` this entry was
    /// declared under with `groupChildMenu`.
    pub group: Option<usize>,
    /// Source line.
    pub line: u32,
}

/// One top-level menu.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    /// Menu label without the surrounding quotes.
    pub title: String,
    /// Items in document order.
    pub items: Vec<MenuItem>,
    /// Window the menu belongs to (`main` when unset).
    pub dialog: Option<String>,
    /// Enable/visible expressions on the `menu =` line, as written.
    pub conditions: Vec<String>,
}

/// `[SettingGroups]`: a named multi-choice whose options become `#if` flags.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SettingGroup {
    /// Group id used in the project snapshot.
    pub name: String,
    /// Display name.
    pub label: String,
    /// Options in declaration order.
    pub options: Vec<SettingOption>,
}

/// One option of a [`SettingGroup`].
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SettingOption {
    /// Flag name the `#if` tests.
    pub name: String,
    /// Display name.
    pub label: String,
}

/// One raw controller command.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ControllerCommand {
    /// Command name.
    pub name: String,
    /// Commands executed in order; envelope bytes excluded.
    pub commands: Vec<Command>,
    /// Description, when `displayCommand` opted the command into the UI.
    pub description: Option<String>,
    /// Source line.
    pub line: u32,
}

/// One sensor calibration table from `[ReferenceTables]`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Default)]
pub struct ReferenceTable {
    /// Table name.
    pub name: String,
    /// Display title.
    pub title: Option<String>,
    /// Target table identifiers and labels.
    pub identifiers: Vec<(u32, String)>,
    /// Number of translated elements.
    pub adc_count: Option<u32>,
    /// Element width in bytes.
    pub bytes_per_adc: Option<u32>,
    /// Scale applied before writing.
    pub scale: Option<Number>,
    /// Optional start offset.
    pub start_offset: Option<u32>,
    /// Per-identifier limits and defaults.
    pub limits: Vec<(u32, Number, Number, Number)>,
    /// Solution labels and expressions, as written.
    pub solutions: Vec<(String, String)>,
    /// Source line.
    pub line: u32,
}

/// One `[Datalog]` field.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DatalogEntry {
    /// Channel name.
    pub channel: String,
    /// Column label.
    pub label: String,
    /// Declared value type (`float` / `int`), legacy.
    pub value_type: Option<String>,
    /// Format string or boolean tag.
    pub format: String,
    /// Enable condition, as written.
    pub enable: Option<String>,
    /// Record lag.
    pub lag: Option<String>,
    /// Category, from the most recent `category =` line.
    pub category: Option<String>,
    /// Source line.
    pub line: u32,
}

/// One hidden-dialog hotkey from `[KeyActions]`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct KeyAction {
    /// Keys in order.
    pub keys: String,
    /// Dialog the keys open.
    pub dialog: String,
    /// Source line.
    pub line: u32,
}

/// A section this build keeps as raw data.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedSection {
    /// Section name as written.
    pub name: String,
    /// Logical lines, in order, comments already removed.
    pub lines: Vec<String>,
}

impl UnsupportedSection {
    /// Renders the section back as INI text.
    ///
    /// Capturing an unknown section is only useful if it can be handed back, so
    /// the round-trip is part of the contract: `[Name]` then the logical lines.
    /// This is **not** byte-exact: preprocessing removed comments and joined
    /// continuations before the lines got here, and unknown keys inside
    /// *modelled* sections are not captured here at all (they are reported once
    /// and preserved in the section's `raw` map where one exists).
    pub fn render(&self) -> String {
        let mut out = format!("[{}]\n", self.name);
        for line in &self.lines {
            out.push_str("  ");
            out.push_str(line);
            out.push('\n');
        }
        out
    }
}

/// Per-section coverage of the definition.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct SectionCoverage {
    /// Section name.
    pub name: String,
    /// Entries seen.
    pub entries: usize,
    /// Entries turned into model data.
    pub understood: usize,
    /// Entries kept as raw data or dropped.
    pub unsupported: usize,
}

/// Percentage of entries understood, 100 when there are none.
#[expect(
    clippy::cast_precision_loss,
    clippy::as_conversions,
    reason = "entry counts are far below 2^53, so f64 represents them exactly"
)]
fn coverage_percent(understood: usize, entries: usize) -> f64 {
    if entries == 0 {
        return 100.0;
    }
    (understood as f64 / entries as f64) * 100.0
}

impl SectionCoverage {
    /// Percentage of entries understood, 100 when the section is empty.
    pub fn percent(&self) -> f64 {
        coverage_percent(self.understood, self.entries)
    }
}

/// Coverage for the whole file.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct Coverage {
    /// One row per section, in document order.
    pub sections: Vec<SectionCoverage>,
}

impl Coverage {
    /// Percentage of all entries understood.
    pub fn percent(&self) -> f64 {
        let entries: usize = self.sections.iter().map(|section| section.entries).sum();
        let understood: usize = self.sections.iter().map(|section| section.understood).sum();
        coverage_percent(understood, entries)
    }

    /// The coverage row for one section.
    pub fn section(&self, name: &str) -> Option<&SectionCoverage> {
        self.sections
            .iter()
            .find(|section| section.name.eq_ignore_ascii_case(name))
    }
}

/// `[ConstantsExtensions]` / `[ConstantExtensions]` metadata.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Extensions {
    /// `defaultValue` entries.
    pub defaults: Ordered<Number>,
    /// `rawValue` entries (defaults in unscaled units).
    pub raw_defaults: Ordered<Number>,
    /// `maximumElements` caps for resizable arrays.
    pub maximum_elements: Ordered<u32>,
    /// Constants the controller owns; never edited locally.
    pub read_only: BTreeSet<String>,
    /// Constants that require a power cycle to take effect.
    pub power_cycle: BTreeSet<String>,
    /// Constants overwritten from the ECU on every connect.
    pub controller_priority: BTreeSet<String>,
    /// Arrays indexed low→high reversed.
    pub reverse_index: BTreeSet<String>,
    /// Constants with a per-entry byte-order flip.
    pub opposite_endian: BTreeSet<String>,
    /// Non-linear scaling tables.
    pub mapping_tables: Ordered<String>,
    /// `maintainConstantValue` rules, as written.
    pub maintain: Vec<(String, String)>,
    /// Whether missing bit options are filled with `INVALID` silently.
    pub ignore_missing_bit_options: bool,
    /// Whether boilerplate is stripped from configuration-error text.
    pub suppress_config_error_verbiage: bool,
}

impl Extensions {
    /// Whether a constant is controller-owned.
    pub fn is_read_only(&self, name: &str) -> bool {
        self.read_only.contains(name)
    }

    /// Whether a change to `name` needs the ECU power-cycled before it takes
    /// effect.
    ///
    /// The dialect writes the name as declared, but constants are matched
    /// case-insensitively elsewhere, so this lookup is too.
    pub fn requires_power_cycle(&self, name: &str) -> bool {
        self.power_cycle
            .iter()
            .any(|entry| entry.eq_ignore_ascii_case(name))
    }
}

/// The frozen result of parsing an INI file.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Definition {
    /// Firmware signature from `[MegaTune]` or `[TunerStudio]`.
    pub signature: String,
    /// Section the signature came from.
    pub signature_section: String,
    /// `iniVersion`, when the file declares one.
    pub ini_version: Option<String>,
    /// `iniSpecVersion`.
    pub spec_version: Option<f64>,
    /// Command whose reply is the signature.
    pub query_command: Option<Command>,
    /// Firmware version text.
    pub version_info: Option<String>,
    /// Byte order of calibration data.
    pub endianness: Endianness,
    /// Pages by number, 1-based.
    pub pages: Vec<Page>,
    /// Communication parameters.
    pub protocol: Protocol,
    /// Calibration constants in load order.
    pub constants: Ordered<Constant>,
    /// PC-side variables in load order.
    pub pc_variables: Ordered<PcVariable>,
    /// Realtime channels in load order.
    pub channels: Ordered<Channel>,
    /// Tables in declaration order.
    pub tables: Ordered<Table>,
    /// Curves in declaration order.
    pub curves: Ordered<Curve>,
    /// Gauge templates, in category order.
    pub gauges: Vec<Gauge>,
    /// Default dashboard.
    pub front_page: FrontPage,
    /// Dialogs in declaration order.
    pub dialogs: Ordered<Dialog>,
    /// Top-level menus.
    pub menus: Vec<Menu>,
    /// Raw controller commands.
    pub controller_commands: Ordered<ControllerCommand>,
    /// Setting groups whose options are `#if` flags.
    pub setting_groups: Ordered<SettingGroup>,
    /// Per-constant help text.
    pub context_help: Ordered<String>,
    /// Datalog fields.
    pub datalog_entries: Vec<DatalogEntry>,
    /// Constant metadata applied after declaration.
    pub extensions: Extensions,
    /// Sensor calibration tables.
    pub reference_tables: Ordered<ReferenceTable>,
    /// Hidden-dialog hotkeys.
    pub key_actions: Vec<KeyAction>,
    /// Sections kept as raw data.
    pub unsupported: Vec<UnsupportedSection>,
    /// `#define` string lists, keyed lowercase.
    pub defines: BTreeMap<String, Vec<String>>,
    /// Flags the file asked about.
    pub tested_flags: BTreeSet<String>,
    /// Flags that were active for this parse.
    pub active_flags: BTreeSet<String>,
    /// Per-section coverage.
    pub coverage: Coverage,
    /// Everything that was adjusted, dropped or inferred.
    pub diagnostics: Vec<Diagnostic>,
}

/// A numeric PC-variable default converted to the byte the command expects.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::as_conversions,
    reason = "PC variables are byte-valued; the cast matches the dialect's truncation"
)]
fn variable_byte(value: f64) -> u8 {
    value as u8
}

impl Definition {
    /// An empty definition, used as a base by the parser.
    pub fn empty() -> Self {
        Self {
            signature: String::new(),
            signature_section: String::new(),
            ini_version: None,
            spec_version: None,
            query_command: None,
            version_info: None,
            endianness: Endianness::default(),
            pages: Vec::new(),
            protocol: Protocol::default(),
            constants: Ordered::new(),
            pc_variables: Ordered::new(),
            channels: Ordered::new(),
            tables: Ordered::new(),
            curves: Ordered::new(),
            gauges: Vec::new(),
            front_page: FrontPage::default(),
            dialogs: Ordered::new(),
            menus: Vec::new(),
            controller_commands: Ordered::new(),
            setting_groups: Ordered::new(),
            context_help: Ordered::new(),
            datalog_entries: Vec::new(),
            extensions: Extensions::default(),
            reference_tables: Ordered::new(),
            key_actions: Vec::new(),
            unsupported: Vec::new(),
            defines: BTreeMap::new(),
            tested_flags: BTreeSet::new(),
            active_flags: BTreeSet::new(),
            coverage: Coverage::default(),
            diagnostics: Vec::new(),
        }
    }

    /// The page with this number, if declared.
    pub fn page(&self, number: u8) -> Option<&Page> {
        self.pages.iter().find(|page| page.number == number)
    }

    /// A constant by name.
    pub fn constant(&self, name: &str) -> Option<&Constant> {
        self.constants.get_ci(name)
    }

    /// A channel by name.
    pub fn channel(&self, name: &str) -> Option<&Channel> {
        self.channels.get_ci(name)
    }

    /// Whether a definition that did not just come from the parser is still
    /// structurally usable.
    ///
    /// This is the gate for a cache hit: deserialization proves the bytes are
    /// a `Definition`, not that they are a *sane* one. The checks are the
    /// invariants every parse enforces and every consumer relies on, not a
    /// second full validation pass — a definition that fails them is rebuilt
    /// from its source instead of shown.
    pub fn is_usable(&self) -> bool {
        !self.signature.is_empty()
            && self
                .spec_version
                .is_none_or(|version| version <= crate::MAX_SPEC_VERSION)
    }

    /// A table by id.
    pub fn table(&self, id: &str) -> Option<&Table> {
        self.tables.get_ci(id)
    }

    /// A raw value substituted for `$name` in a command.
    ///
    /// PC variables are byte-valued; a `bits` variable's value is the index of
    /// its selected option, which is how `tsCanId` becomes `0x00`.
    pub fn command_variable(&self, name: &str) -> Option<u8> {
        let variable = self.pc_variables.get_ci(name)?;
        if let Some(value) = self
            .extensions
            .defaults
            .get_ci(name)
            .and_then(Number::literal)
        {
            return Some(variable_byte(value));
        }
        match variable.class {
            ConstantClass::Bits => variable
                .options
                .iter()
                .find(|option| !option.hidden)
                .and_then(|option| u8::try_from(option.value).ok()),
            _ => Some(0),
        }
    }

    /// Diagnostics at [`crate::error::Severity::Error`] or above.
    pub fn errors(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == crate::error::Severity::Error)
    }

    /// A stable, human-readable rendering of the parsed model.
    ///
    /// Golden tests compare this text, so a change in what the parser
    /// understands shows up as a reviewable diff rather than a silently
    /// different count. Order is the definition's own order, never a map's.
    pub fn summary(&self) -> String {
        let mut out = String::new();
        self.write_identity(&mut out);
        self.write_pages(&mut out);
        self.write_constants(&mut out);
        self.write_channels(&mut out);
        self.write_tables(&mut out);
        self.write_tail(&mut out);
        out
    }

    fn write_identity(&self, out: &mut String) {
        let _ = writeln!(out, "signature|{}", self.signature);
        let _ = writeln!(out, "signature_section|{}", self.signature_section);
        let _ = writeln!(out, "spec|{:?}", self.spec_version);
        let _ = writeln!(out, "endianness|{}", self.endianness);
        let flags = self
            .active_flags
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(",");
        let _ = writeln!(out, "flags|{flags}");
    }

    fn write_pages(&self, out: &mut String) {
        let _ = writeln!(out, "pages|{}", self.pages.len());
        for page in &self.pages {
            let _ = writeln!(
                out,
                "page|{}|size={}|burn={}|read={}|writable={}",
                page.number,
                page.size,
                command_text(page.burn_command.as_ref()),
                command_text(page.read_command.as_ref()),
                page.is_writable()
            );
        }
    }

    fn write_constants(&self, out: &mut String) {
        let _ = writeln!(out, "constants|{}", self.constants.len());
        for (name, constant) in self.constants.iter() {
            let _ = writeln!(
                out,
                "constant|{name}|{:?}|{}|page={}|addr={:?}|shape={}|units={}|scale={}|translate={}",
                constant.class,
                constant.data_type,
                constant.page,
                constant.address,
                constant.shape,
                constant.units,
                constant
                    .scale
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default(),
                constant
                    .translate
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_default()
            );
        }
        let _ = writeln!(out, "pc_variables|{}", self.pc_variables.len());
    }

    fn write_channels(&self, out: &mut String) {
        let _ = writeln!(out, "channels|{}", self.channels.len());
        for (name, channel) in self.channels.iter() {
            let kind = match &channel.kind {
                ChannelKind::Computed(_) => "computed".to_owned(),
                ChannelKind::Measured {
                    data_type,
                    offset,
                    bits,
                    ..
                } => match bits {
                    Some(bits) => {
                        format!("bits{}({}..{})@{offset}", bits.width(), bits.low, bits.high)
                    }
                    None => format!("{data_type}@{offset}"),
                },
            };
            let _ = writeln!(out, "channel|{name}|{kind}|units={}", channel.units);
        }
    }

    fn write_tables(&self, out: &mut String) {
        let _ = writeln!(out, "tables|{}", self.tables.len());
        for (id, table) in self.tables.iter() {
            let _ = writeln!(
                out,
                "table|{id}|x={}|y={}|z={}|title={}",
                table.x_bins.as_ref().map_or("", |axis| axis.array.as_str()),
                table.y_bins.as_ref().map_or("", |axis| axis.array.as_str()),
                table.z_bins.as_deref().unwrap_or(""),
                table.title
            );
        }
    }

    fn write_tail(&self, out: &mut String) {
        let _ = writeln!(out, "curves|{}", self.curves.len());
        let _ = writeln!(out, "gauges|{}", self.gauges.len());
        let _ = writeln!(out, "dialogs|{}", self.dialogs.len());
        let _ = writeln!(out, "menus|{}", self.menus.len());
        let _ = writeln!(out, "commands|{}", self.controller_commands.len());
        let _ = writeln!(out, "datalog|{}", self.datalog_entries.len());
        let _ = writeln!(out, "unsupported|{}", self.unsupported.len());
        for section in &self.unsupported {
            let _ = writeln!(out, "unsupported_section|{}", section.name);
        }
        let _ = writeln!(out, "coverage|{:.2}", self.coverage.percent());
        let errors = self
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == crate::error::Severity::Error)
            .count();
        let warnings = self
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == crate::error::Severity::Warning)
            .count();
        let _ = writeln!(out, "errors|{errors}");
        let _ = writeln!(out, "warnings|{warnings}");
    }
}

fn command_text(command: Option<&Command>) -> String {
    match command {
        Some(command) if !command.is_empty() => command.raw.clone(),
        Some(_) => "<empty>".to_owned(),
        None => "<none>".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

        assert_eq!(constants.get_ci("REQFUEL"), Some(&1));
        assert_eq!(constants.get("REQFUEL"), None);
    }

    #[test]
    fn a_case_insensitive_lookup_returns_the_first_declaration() {
        let mut constants: Ordered<u32> = Ordered::new();
        constants.insert("reqFuel", 1);
        constants.insert("reqfuel", 2);
        constants.insert("REQFUEL", 3);

        assert_eq!(constants.get_ci("ReqFuel"), Some(&1));
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
            constants.get_ci("spaced"),
            None,
            "a stored name keeps its spaces, matching the linear scan this replaced"
        );
        assert_eq!(constants.get_ci("  spaced  "), None);
    }

    #[test]
    fn redefining_a_name_keeps_order_and_the_index_consistent() {
        let mut constants: Ordered<u32> = Ordered::new();
        constants.insert("first", 1);
        constants.insert("second", 2);
        constants.insert("First", 3);
        constants.insert("first", 10);

        assert_eq!(constants.get_ci("FIRST"), Some(&10));
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
                            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(query.trim()))
                            .map(|(_, value)| value)
                    })
                    .copied();

                proptest::prop_assert_eq!(ordered.get_ci(&query).copied(), expected);
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
            units: Text::Literal("%".to_owned()),
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

        page.burn_command = Some(Command::parse(r#""""#));
        assert!(!page.is_burnable());

        page.burn_command = Some(Command::parse(r#""B%2i""#));
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
}
