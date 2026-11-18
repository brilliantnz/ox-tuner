//! Raw ↔ user conversion and the value storage form.
//!
//! The dialect defines user values in one line (reference §Scale / translate):
//!
//! ```text
//! user = (raw + translate) × scale
//! raw  = user / scale − translate
//! ```
//!
//! Three rules keep this honest:
//!
//! - **Integer storage rounds; it never truncates.** `U08` raw 10.5 becomes 11
//!   (half away from zero) and the fact that rounding happened is reported.
//! - **A user's edit is flagged, never clamped.** Clamping an edit is the
//!   core's S2 guardrail (ADR-0011); silently changing it here would hide both
//!   the edit and the guardrail.
//! - **A value at a project-file boundary is clamped.** The file has to carry
//!   what the definition declares and the storage type holds, so
//!   [`VariableSpec::clamp_raw`] keeps the round trip intact instead of
//!   writing a value the next open would change.

use std::fmt;

use oxtuner_ini::DataType;

use crate::{EntryShape, VariableSpec};

/// A stored value vector.
///
/// Scalars and bit fields hold one number; arrays hold their elements in
/// row-major order; text holds the decoded string.
#[expect(
    clippy::exhaustive_enums,
    reason = "a stored value is either numeric or text; the set cannot grow"
)]
#[derive(Debug, Clone, PartialEq)]
pub enum Values {
    /// Raw numeric values.
    Numbers(Vec<f64>),
    /// Text, without the surrounding quotes of the file.
    Text(String),
}

impl Values {
    /// A numeric vector of `count` copies of `value`.
    pub fn numbers(value: f64, count: usize) -> Self {
        Self::Numbers(vec![value; count])
    }

    /// Number of stored elements. Text counts as one value.
    pub const fn len(&self) -> usize {
        match self {
            Self::Numbers(values) => values.len(),
            Self::Text(_) => 1,
        }
    }

    /// Whether the vector has no elements.
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// One raw number.
    pub fn number(&self, index: usize) -> Option<f64> {
        match self {
            Self::Numbers(values) => values.get(index).copied(),
            Self::Text(_) => None,
        }
    }

    /// The text, when this is a text value.
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Numbers(_) => None,
        }
    }

    /// Number of elements that differ from `other`.
    ///
    /// Text values compare whole strings and count as one cell.
    #[expect(
        clippy::float_cmp,
        reason = "a cell is dirty only when its stored float differs exactly"
    )]
    pub fn changed_cells(&self, other: &Self) -> usize {
        match (self, other) {
            (Self::Numbers(left), Self::Numbers(right)) => left
                .iter()
                .zip(right)
                .filter(|(left, right)| left != right)
                .count(),
            (Self::Text(left), Self::Text(right)) => usize::from(left != right),
            _ => self.len().max(other.len()),
        }
    }

    /// Whether the element at `index` equals the same element of `other`.
    ///
    /// Text values compare whole strings; the index is the single text cell.
    #[expect(
        clippy::float_cmp,
        reason = "cell equality is exact; approximate equality would hide edits"
    )]
    pub fn same_at(&self, other: &Self, index: usize) -> bool {
        match (self, other) {
            (Self::Numbers(left), Self::Numbers(right)) => {
                match (left.get(index), right.get(index)) {
                    (Some(left), Some(right)) => left == right,
                    _ => false,
                }
            }
            (Self::Text(left), Self::Text(right)) => index == 0 && left == right,
            _ => false,
        }
    }
}

/// The result of turning a user value into the raw value the ECU sees.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawConversion {
    /// The raw value to store.
    pub raw: f64,
    /// Whether a fractional raw value had to be rounded for integer storage.
    pub rounded: bool,
    /// Whether the user value is outside the definition's `min`/`max`.
    pub user_out_of_range: bool,
    /// Whether the raw value does not fit the declared storage type.
    pub raw_out_of_range: bool,
}

impl RawConversion {
    /// Whether anything about the conversion deserves the user's attention.
    pub const fn is_out_of_range(self) -> bool {
        self.user_out_of_range || self.raw_out_of_range
    }
}

/// Why a value could not be converted.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttributeError {
    /// The definition wrote an expression for `attribute` that this build could
    /// not evaluate against the loaded tune.
    Unresolved {
        /// The variable's name.
        name: String,
        /// `scale`, `translate`, ….
        attribute: &'static str,
    },
    /// The definition's scale is zero, which has no inverse.
    ZeroScale {
        /// The variable's name.
        name: String,
    },
}

impl fmt::Display for AttributeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unresolved { name, attribute } => write!(
                formatter,
                "`{name}`: the `{attribute}` attribute is an unresolved expression, so the value \
                 cannot be converted"
            ),
            Self::ZeroScale { name } => {
                write!(formatter, "`{name}`: scale is 0, so no user value exists")
            }
        }
    }
}

impl std::error::Error for AttributeError {}

impl VariableSpec {
    /// Converts a raw value to user units.
    ///
    /// # Errors
    ///
    /// Returns [`AttributeError::Unresolved`] when `scale` or `translate` is an
    /// expression this build cannot evaluate.
    pub fn to_user(&self, raw: f64) -> Result<f64, AttributeError> {
        let scale = self.scale.ok_or_else(|| AttributeError::Unresolved {
            name: self.name.clone(),
            attribute: "scale",
        })?;
        let translate = self.translate.ok_or_else(|| AttributeError::Unresolved {
            name: self.name.clone(),
            attribute: "translate",
        })?;
        Ok((raw + translate) * scale)
    }

    /// Converts a user value to raw storage.
    ///
    /// Integer types round half away from zero. Nothing is clamped: the caller
    /// decides what an out-of-range edit means (ADR-0011 S2 lives in the core).
    /// A file boundary uses [`Self::clamp_raw`] instead.
    ///
    /// # Errors
    ///
    /// Returns [`AttributeError::Unresolved`] for an unevaluable attribute and
    /// [`AttributeError::ZeroScale`] when the scale has no inverse.
    pub fn to_raw(&self, user: f64) -> Result<RawConversion, AttributeError> {
        let scale = self.scale.ok_or_else(|| AttributeError::Unresolved {
            name: self.name.clone(),
            attribute: "scale",
        })?;
        let translate = self.translate.ok_or_else(|| AttributeError::Unresolved {
            name: self.name.clone(),
            attribute: "translate",
        })?;
        if scale == 0.0 {
            return Err(AttributeError::ZeroScale {
                name: self.name.clone(),
            });
        }

        let computed = user / scale - translate;
        let integer = is_integer(self.data_type);
        let raw = if integer { computed.round() } else { computed };

        let user_out_of_range = self.minimum.is_some_and(|minimum| user < minimum)
            || self.maximum.is_some_and(|maximum| user > maximum);
        let (lowest, highest) = storage_range(self.data_type);
        let raw_out_of_range = !raw.is_finite() || raw < lowest || raw > highest;

        Ok(RawConversion {
            raw,
            rounded: integer && computed.fract() != 0.0,
            user_out_of_range,
            raw_out_of_range,
        })
    }

    /// Pulls a raw value inside the declared range and the storage type.
    ///
    /// A default or a file value outside either would otherwise read back
    /// changed. The declared bounds are first pulled in to the decimals a file
    /// writes: a minimum of `7.25` written with one decimal reads back as
    /// `7.2`, below the minimum again. A declared range narrower than one
    /// written step keeps its own bounds and leaves the text alone.
    ///
    /// The storage type narrows here instead of keeping and warning, unlike
    /// [`Self::to_raw`]: a file boundary has to write something the type can
    /// hold, and the alternative is a value that changes on the next open.
    /// A non-finite value has no sane position in the range, so it becomes
    /// the storage type's own zero.
    #[must_use]
    pub fn clamp_raw(&self, raw: f64) -> f64 {
        let (storage_low, storage_high) = storage_range(self.data_type);
        if !raw.is_finite() {
            return 0.0_f64.clamp(storage_low, storage_high);
        }

        // An attribute that cannot be evaluated leaves only the storage
        // clamp; the caller still gets a value the type can hold.
        let Some(user) = self.to_user(raw).ok() else {
            return raw.clamp(storage_low, storage_high);
        };

        let declared_low = self.minimum.unwrap_or(f64::NEG_INFINITY);
        let declared_high = self.maximum.unwrap_or(f64::INFINITY);
        let (declared_low, declared_high) = (
            declared_low.min(declared_high),
            declared_low.max(declared_high),
        );

        let step = 10f64.powi(-i32::from(self.digits.max(1)));
        let low = {
            let steps = declared_low / step;
            let epsilon = f64::EPSILON * 8.0 * steps.abs().max(1.0);
            ((steps - epsilon).ceil() * step).max(declared_low)
        };
        let high = {
            let steps = declared_high / step;
            let epsilon = f64::EPSILON * 8.0 * steps.abs().max(1.0);
            ((steps + epsilon).floor() * step).min(declared_high)
        };
        let user = if low <= high {
            user.clamp(low, high)
        } else {
            user.clamp(declared_low, declared_high)
        };

        let raw = self.to_raw(user).map_or(raw, |conversion| conversion.raw);

        raw.clamp(storage_low, storage_high)
    }

    /// Formats a user value with the definition's digit count.
    pub fn format_user(&self, user: f64) -> String {
        format_fixed(normalize_zero(user), usize::from(self.digits))
    }

    /// Formats a raw value as the user-unit text a calibration file carries.
    ///
    /// `TunerStudio` always writes at least one decimal (`12000.0`), so this
    /// does too; the definition's digits win when they are larger.
    ///
    /// # Errors
    ///
    /// Returns [`AttributeError`] when the raw value cannot be converted to
    /// user units.
    pub fn format_file(&self, raw: f64) -> Result<String, AttributeError> {
        let user = self.to_user(raw)?;
        let digits = usize::from(self.digits).max(1);
        Ok(format_fixed(normalize_zero(user), digits))
    }

    /// The shape as a human-readable label (`scalar`, `bits[0:5]`, `16×16`).
    pub fn shape_label(&self) -> String {
        match self.shape {
            EntryShape::Scalar => "scalar".to_owned(),
            EntryShape::Bits { low, high, shift } => {
                if shift == 0 {
                    format!("bits[{low}:{high}]")
                } else {
                    format!("bits[{low}:{high}+{shift}]")
                }
            }
            EntryShape::Array { cols, rows: 1 } => format!("{cols} values"),
            EntryShape::Array { cols, rows } => format!("{cols}×{rows}"),
            EntryShape::Text { len } => format!("{len} chars"),
        }
    }
}

fn normalize_zero(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}

fn format_fixed(value: f64, digits: usize) -> String {
    format!("{value:.digits$}")
}

/// Whether the storage type holds integers.
pub const fn is_integer(data_type: DataType) -> bool {
    !matches!(data_type, DataType::F32 | DataType::Ascii)
}

/// Inclusive range of raw values the storage type can hold.
///
/// `M08` is unsigned storage that *displays* negatives past its maximum; the
/// bytes are still 0–255, so that is the range used here.
#[expect(
    clippy::cast_precision_loss,
    clippy::as_conversions,
    reason = "the model stores raw values as f64; 64-bit bounds are therefore approximate"
)]
pub fn storage_range(data_type: DataType) -> (f64, f64) {
    match data_type {
        DataType::U08 | DataType::M08 | DataType::Ascii => (0.0, 255.0),
        DataType::S08 => (-128.0, 127.0),
        DataType::U16 => (0.0, 65_535.0),
        DataType::S16 => (-32_768.0, 32_767.0),
        DataType::U32 => (0.0, 4_294_967_295.0),
        DataType::S32 => (-2_147_483_648.0, 2_147_483_647.0),
        // f64 cannot represent every u64/i64 value; the tune model stores raw
        // values as f64 (documented limitation) and the range is the widest
        // interval f64 can name without pretending to be exact.
        DataType::U64 => (0.0, u64::MAX as f64),
        DataType::S64 => (i64::MIN as f64, i64::MAX as f64),
        DataType::F32 => (f64::NEG_INFINITY, f64::INFINITY),
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "unit tests may unwrap; a failed call is a failed test"
    )]
    use super::*;
    use crate::Owner;

    fn scalar(data_type: DataType, scale: f64, translate: f64) -> VariableSpec {
        VariableSpec {
            name: "value".to_owned(),
            owner: Owner::Page(crate::IniPage::FIRST),
            shape: EntryShape::Scalar,
            data_type,
            address: None,
            units: String::new(),
            scale: Some(scale),
            translate: Some(translate),
            minimum: None,
            maximum: None,
            digits: 0,
            controller_owned: false,
            no_msq_save: false,
            default_raw: 0.0,
        }
    }

    #[test]
    fn the_documented_formula_holds_in_both_directions() {
        let spec = scalar(DataType::U08, 0.1, 0.0);

        assert!((spec.to_user(147.0).unwrap() - 14.7).abs() < 1e-12);
        assert_eq!(spec.to_raw(14.7).unwrap().raw, 147.0);
    }

    #[test]
    fn translate_is_applied_before_scale() {
        let spec = scalar(DataType::U08, 0.5, -10.0);

        // raw 30 → (30 − 10) × 0.5 = 10
        assert_eq!(spec.to_user(30.0).unwrap(), 10.0);
        // user 10 → 10 / 0.5 + 10 = 30
        assert_eq!(spec.to_raw(10.0).unwrap().raw, 30.0);
    }

    #[test]
    fn integer_storage_rounds_half_away_from_zero_and_says_so() {
        let spec = scalar(DataType::U08, 1.0, 0.0);

        let up = spec.to_raw(10.5).unwrap();
        assert_eq!(up.raw, 11.0);
        assert!(up.rounded);

        let down = spec.to_raw(-10.5).unwrap();
        assert_eq!(down.raw, -11.0);

        let exact = spec.to_raw(10.0).unwrap();
        assert!(!exact.rounded);
    }

    #[test]
    fn out_of_range_is_flagged_not_clamped() {
        let mut spec = scalar(DataType::U08, 1.0, 0.0);
        spec.minimum = Some(0.0);
        spec.maximum = Some(255.0);

        let high = spec.to_raw(300.0).unwrap();
        assert_eq!(high.raw, 300.0, "the value is preserved");
        assert!(high.user_out_of_range);
        assert!(high.raw_out_of_range);

        let low = spec.to_raw(-1.0).unwrap();
        assert_eq!(low.raw, -1.0);
        assert!(low.user_out_of_range);
        assert!(low.raw_out_of_range);
    }

    #[test]
    fn a_zero_scale_is_an_error_not_a_division_by_zero() {
        let spec = scalar(DataType::U08, 0.0, 0.0);
        assert_eq!(
            spec.to_raw(1.0),
            Err(AttributeError::ZeroScale {
                name: "value".to_owned()
            })
        );
    }

    #[test]
    fn an_unresolved_scale_cannot_convert() {
        let mut spec = scalar(DataType::U08, 1.0, 0.0);
        spec.scale = None;

        assert!(matches!(
            spec.to_user(1.0),
            Err(AttributeError::Unresolved {
                attribute: "scale",
                ..
            })
        ));
        assert!(spec.to_raw(1.0).is_err());
    }

    #[test]
    fn formatting_uses_the_definition_digits() {
        let mut spec = scalar(DataType::U08, 0.1, 0.0);
        spec.digits = 1;
        assert_eq!(spec.format_user(14.7), "14.7");

        spec.digits = 3;
        assert_eq!(spec.format_user(14.7), "14.700");
        assert_eq!(spec.format_user(-0.0), "0.000");
    }

    #[test]
    fn a_file_value_always_carries_a_decimal() {
        let spec = scalar(DataType::U08, 1.0, 0.0);
        assert_eq!(spec.format_file(120.0).unwrap(), "120.0");
    }

    #[test]
    fn storage_ranges_cover_every_declared_type() {
        assert_eq!(storage_range(DataType::U08), (0.0, 255.0));
        assert_eq!(storage_range(DataType::S08), (-128.0, 127.0));
        assert_eq!(storage_range(DataType::U16), (0.0, 65_535.0));
        assert_eq!(storage_range(DataType::S16), (-32_768.0, 32_767.0));
        assert!(storage_range(DataType::F32).0.is_infinite());
    }

    #[test]
    fn text_values_compare_as_one_cell() {
        let left = Values::Text("Even fire".to_owned());
        let right = Values::Text("Even fire".to_owned());
        let other = Values::Text("Odd fire".to_owned());

        assert_eq!(left.len(), 1);
        assert!(left.same_at(&right, 0));
        assert!(!left.same_at(&other, 0));
        assert_eq!(left.text(), Some("Even fire"));
        assert_eq!(left.number(0), None);
    }
}
