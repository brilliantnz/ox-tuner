use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use crate::expr::Expr;

thread_local! {
    /// Per-parse interner for literal text. The parser clears it when the parse
    /// leaves, so a 65k-field definition holds one copy of each repeated unit
    /// or title without retaining the table afterwards.
    static TEXT_CACHE: RefCell<HashMap<Arc<str>, Arc<str>>> = RefCell::new(HashMap::new());
}

/// Returns the shared copy of `value`, allocating it only on first use.
pub(crate) fn intern_text(value: &str) -> Arc<str> {
    TEXT_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(found) = cache.get(value) {
            return Arc::clone(found);
        }
        let shared: Arc<str> = Arc::from(value);
        cache.insert(Arc::clone(&shared), Arc::clone(&shared));
        shared
    })
}

/// Drops this parse's text intern table.
pub(crate) fn clear_text_cache() {
    TEXT_CACHE.with(|cache| cache.borrow_mut().clear());
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

    /// Canonical form for comparisons, independent of how the value was written.
    ///
    /// Consumers comparing two attributes should use this rather than
    /// [`Self::Display`]; a future definition cache key can use it too. Unlike
    /// `Display`, it has no braces around a dynamic expression's source.
    ///
    /// [`Self::Display`] round-trips the source text, which is what golden
    /// summaries need. Two attributes can still mean the same number while
    /// being written differently (`2` and `{ 1 + 1 }`). This folds expressions
    /// that depend on nothing but literals and pure builtins to one number, and
    /// leaves everything else as its source. It never guesses: an unresolved
    /// name or a call this build cannot fold keeps its source form.
    pub fn normalized(&self) -> String {
        match self {
            Self::Literal(value) => format_literal(*value),
            Self::Expression { source, .. } => self
                .evaluate(&|_| None)
                .map_or_else(|| source.clone(), format_literal),
        }
    }
}

/// Largest magnitude a `f64` still displays as an exact integer.
///
/// Above this bound the literal would print in exponent form, so the display
/// keeps the decimal text as written instead.
const MAX_EXACT_INTEGER: f64 = 1e15;

/// A whole number in the range the display form treats as an integer.
#[expect(
    clippy::cast_possible_truncation,
    clippy::as_conversions,
    reason = "the caller checks fract() == 0 and abs() < MAX_EXACT_INTEGER before converting"
)]
fn whole_number(value: f64) -> i64 {
    value as i64
}

/// The one numeric text form shared by [`Number::Display`] and
/// [`Number::normalized`], so a comparison never disagrees with a printout.
fn format_literal(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < MAX_EXACT_INTEGER {
        whole_number(value).to_string()
    } else {
        value.to_string()
    }
}

impl fmt::Display for Number {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Literal(value) => formatter.write_str(&format_literal(*value)),
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
    /// Plain text, quotes removed. Repeated values share one allocation.
    Literal(Arc<str>),
    /// A `{ … }` expression that produces text at runtime.
    Expression {
        /// The expression as written.
        source: String,
        /// `None` when it could not be parsed.
        parsed: Option<Expr>,
    },
}

impl Text {
    /// Shared literal text. One allocation per distinct value per parse; the
    /// call is cheap outside a parse.
    pub fn shared(value: impl AsRef<str>) -> Self {
        Self::Literal(intern_text(value.as_ref()))
    }

    /// The shared empty literal.
    pub fn empty() -> Self {
        Self::shared("")
    }

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
        Self::shared(crate::fields::unquote(field))
    }

    /// The literal text, when there is one.
    pub fn literal(&self) -> Option<&str> {
        match self {
            Self::Literal(text) => Some(text.as_ref()),
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
    ///
    /// The source is kept beside the parsed tree so [`Self::Display`] round-trips
    /// the written form instead of printing a placeholder.
    Expression {
        /// The expression as written, without braces.
        source: String,
        /// The parsed tree.
        parsed: Expr,
    },
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
            Self::Expression { .. } => None,
        }
    }

    /// Folds the value with `lookup` supplying names.
    pub fn evaluate(&self, lookup: &dyn Fn(&str) -> Option<f64>) -> Option<u32> {
        match self {
            Self::Fixed(value) => Some(*value),
            Self::Expression { parsed, .. } => parsed
                .evaluate(lookup)
                .filter(|value| *value >= 0.0)
                .map(dimension_value),
        }
    }

    /// Canonical form for comparisons, the same way [`Number::normalized`]
    /// works for numeric attributes. A pure constant expression folds to a
    /// number; anything that needs runtime values keeps its source.
    pub fn normalized(&self) -> String {
        match self {
            Self::Fixed(value) => value.to_string(),
            Self::Expression { source, parsed } => parsed
                .evaluate(&|_| None)
                .filter(|value| *value >= 0.0)
                .map_or_else(
                    || source.clone(),
                    |value| dimension_value(value).to_string(),
                ),
        }
    }
}

impl fmt::Display for Dimension {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fixed(value) => write!(formatter, "{value}"),
            Self::Expression { source, .. } => write!(formatter, "{{{source}}}"),
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
    /// A bit field of 32 or more bits saturates at `u32::MAX` options instead.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn number_display_round_trips_the_expression_source() {
        let number = Number::parse(" { fuel_cols + 1 } ");

        assert_eq!(number.to_string(), "{fuel_cols + 1}");
        assert_eq!(Number::parse(&number.to_string()), number);
    }

    #[test]
    fn normalized_folds_pure_expressions_but_keeps_dynamic_sources() {
        assert_eq!(Number::parse("{ 1 + 1 }").normalized(), "2");
        assert_eq!(
            Number::parse("{ fuel_cols + 1 }").normalized(),
            "fuel_cols + 1"
        );
    }

    #[test]
    fn dimension_display_round_trips_the_written_expression() {
        let dimension = Dimension::Expression {
            source: "fuel_cols * 2".to_owned(),
            parsed: crate::expr::Expr::parse("fuel_cols * 2").expect("the expression parses"),
        };

        assert_eq!(dimension.to_string(), "{fuel_cols * 2}");
        assert_eq!(dimension.normalized(), "fuel_cols * 2");
    }
}
