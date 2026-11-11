use std::fmt;

use crate::expr::Expr;

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
        match value.trim_ascii().to_ascii_lowercase().as_str() {
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
        Some(match value.trim_ascii().to_ascii_uppercase().as_str() {
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
        let field = field.trim_ascii();
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

    /// Folds the value against a full evaluation context.
    ///
    /// This is [`Self::evaluate`] plus arrays and definition-defined
    /// functions, which presentation attributes such as
    /// `{ getChannelScaleByOffset(6) }` need.
    pub fn evaluate_with(&self, context: &mut dyn crate::expr::EvalContext) -> Option<f64> {
        match self {
            Self::Literal(value) => Some(*value),
            Self::Expression { parsed, .. } => parsed.as_ref()?.evaluate_with(context),
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
        let field = field.trim_ascii();
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
    /// Number of elements, when the dimensions are fixed and the product fits
    /// in a `u32`.
    ///
    /// A fixed `[cols x rows]` whose product overflows `u32` reports `None`
    /// even though [`Shape::is_dynamic`] is `false`. The parser reports that
    /// case as `shape-too-large`, so consumers must not read `None` as "empty".
    /// A bit field wider than 32 bits saturates at `u32::MAX` options instead.
    pub fn element_count(&self) -> Option<u32> {
        match self {
            Self::Scalar => Some(1),
            Self::Bits { low, high, .. } => {
                let width = high.saturating_sub(*low).saturating_add(1);
                Some(1u32.checked_shl(u32::from(width)).unwrap_or(u32::MAX))
            }
            Self::List(length) | Self::Text { length, .. } => length.value(),
            Self::Table { cols, rows } => Some(cols.value()?.checked_mul(rows.value()?)?),
        }
    }

    /// Whether any dimension still needs runtime resolution.
    ///
    /// `false` does not guarantee that [`Shape::element_count`] is `Some`:
    /// fixed dimensions can still overflow the element count.
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
