use std::sync::Arc;

use super::{DataType, Number, Shape, Text};
use crate::expr::Expr;

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
        Some(match value.trim_ascii().to_ascii_lowercase().as_str() {
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
    /// Size in bytes, when the shape is known and its element count fits in a
    /// `u32`.
    ///
    /// `None` means the size is unknown: a dimension is still an expression, the
    /// fixed element count times the type width overflows `u32` (reported as
    /// `shape-too-large`), or a `string` entry carries a shape that is not text.
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
