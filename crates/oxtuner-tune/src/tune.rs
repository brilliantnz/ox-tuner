//! The working tune: one value vector per variable plus edit bookkeeping.
//!
//! A [`Tune`] is a plain value — cloneable, comparable by value, and free of
//! I/O. It does **not** know about the ECU, the file or the UI. Images combine
//! two or more tunes ([`crate::TuneImages`]); the core executes write intents.
//!
//! Values are stored in **raw** units (the integer the ECU sees). User units are
//! computed through the layout's scale/translate on every access
//! ([`crate::VariableSpec::to_user`]). That is the only place the conversion
//! exists, so a table cell and a scalar cannot disagree about it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::SystemTime;

use oxtuner_ini::Definition;

use crate::{AttributeError, EntryShape, IniPage, RawConversion, TuneLayout, Values, VariableSpec};

/// One edited cell, for the edit log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Cell {
    variable: usize,
    index: usize,
}

/// What an accepted edit did.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EditOutcome {
    /// The raw value that was stored.
    pub raw: f64,
    /// Whether integer storage had to round a fractional raw value.
    pub rounded: bool,
    /// Whether the value is outside the definition's range (stored anyway).
    pub out_of_range: bool,
}

impl From<RawConversion> for EditOutcome {
    fn from(conversion: RawConversion) -> Self {
        Self {
            raw: conversion.raw,
            rounded: conversion.rounded,
            out_of_range: conversion.is_out_of_range(),
        }
    }
}

/// Why a tune operation could not be performed.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub enum TuneError {
    /// No variable of that name exists in the layout.
    UnknownVariable(String),
    /// A numeric operation was asked for a text variable, or the reverse.
    WrongKind {
        /// The variable's name.
        name: String,
        /// What the caller needed (`number` or `text`).
        expected: &'static str,
    },
    /// A `(row, column)` is outside the variable's shape.
    CellOutOfRange {
        /// The variable's name.
        name: String,
        /// Requested row.
        row: usize,
        /// Requested column.
        column: usize,
    },
    /// A flat index is outside the variable's shape.
    IndexOutOfRange {
        /// The variable's name.
        name: String,
        /// Requested index.
        index: usize,
    },
    /// The definition's presentation attributes could not be resolved.
    Conversion {
        /// Why.
        source: AttributeError,
    },
    /// Two tunes whose value vectors do not match one layout were combined.
    LayoutMismatch,
    /// The layout has no page with that number.
    UnknownPage {
        /// The page asked for.
        page: IniPage,
    },
    /// A page image is shorter than the definition's page size.
    ShortPage {
        /// The page asked for.
        page: IniPage,
        /// Bytes supplied.
        got: usize,
        /// Bytes the definition declares.
        expected: usize,
    },
    /// The definition's page is larger than the codec will build.
    PageTooLarge {
        /// The page asked for.
        page: IniPage,
        /// Bytes the definition declares.
        size: usize,
        /// Largest page the codec accepts.
        limit: usize,
    },
    /// A changed text value contains a character ISO-8859-1 cannot carry.
    TextNotLatin1 {
        /// The variable being written.
        name: String,
        /// The first character that does not fit.
        character: char,
    },
}

impl std::fmt::Display for TuneError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownVariable(name) => write!(formatter, "no tune variable named `{name}`"),
            Self::WrongKind { name, expected } => {
                write!(formatter, "`{name}` is not a {expected} value")
            }
            Self::CellOutOfRange { name, row, column } => {
                write!(formatter, "`{name}` has no cell ({row}, {column})")
            }
            Self::IndexOutOfRange { name, index } => {
                write!(formatter, "`{name}` has no element {index}")
            }
            Self::Conversion { source } => write!(formatter, "{source}"),
            Self::LayoutMismatch => formatter
                .write_str("the tunes' values do not match one layout and cannot be combined"),
            Self::UnknownPage { page } => write!(formatter, "the layout has no page {page}"),
            Self::PageTooLarge { page, size, limit } => write!(
                formatter,
                "page {page} declares {size} byte(s), over the {limit} byte limit"
            ),
            Self::ShortPage {
                page,
                got,
                expected,
            } => write!(
                formatter,
                "page {page} arrived with {got} of {expected} bytes"
            ),
            Self::TextNotLatin1 { name, character } => write!(
                formatter,
                "`{name}` contains {character:?}, which has no ISO-8859-1 byte"
            ),
        }
    }
}

impl std::error::Error for TuneError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Conversion { source } => Some(source),
            _ => None,
        }
    }
}

impl From<AttributeError> for TuneError {
    fn from(source: AttributeError) -> Self {
        Self::Conversion { source }
    }
}

/// The working image of a tune.
#[derive(Debug, Clone)]
pub struct Tune {
    layout: Arc<TuneLayout>,
    values: Vec<Values>,
    edits: BTreeMap<Cell, SystemTime>,
}

impl Tune {
    /// A tune built from a definition with every value at its default.
    pub fn from_definition(definition: &Definition) -> Self {
        let layout = TuneLayout::build(definition, &|_| None);
        Self::new(Arc::new(layout))
    }

    /// A tune whose values are the layout's defaults.
    pub fn new(layout: Arc<TuneLayout>) -> Self {
        let values = layout
            .variables()
            .iter()
            .map(default_values)
            .collect::<Vec<_>>();
        Self {
            layout,
            values,
            edits: BTreeMap::new(),
        }
    }

    /// The frozen layout every value is interpreted through.
    pub fn layout(&self) -> &Arc<TuneLayout> {
        &self.layout
    }

    /// Whether both tunes can be compared value by value.
    pub fn same_layout(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.layout, &other.layout)
    }

    /// Copies every value from `other` into this tune, keeping this tune's
    /// layout.
    ///
    /// An imported `.msq` is parsed with the same definition but builds its own
    /// layout allocation, so a pointer comparison cannot accept it. This is the
    /// checked adoption: every variable must exist with the same element count
    /// and the same value kind; only then are the values replaced. Edit
    /// timestamps do not survive, which is right for a tune that arrived from a
    /// file.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError::LayoutMismatch`] when the value vectors do not
    /// match in length or kind, and [`TuneError::UnknownVariable`] when a
    /// variable is missing.
    pub fn adopt_values(&mut self, other: &Self) -> Result<(), TuneError> {
        if self.layout.variables().len() != other.layout.variables().len() {
            return Err(TuneError::LayoutMismatch);
        }
        let mut values = Vec::with_capacity(self.values.len());
        for spec in self.layout.variables() {
            let Some(other_values) = other.value(&spec.name) else {
                return Err(TuneError::UnknownVariable(spec.name.clone()));
            };
            if other_values.len() != spec.element_count() {
                return Err(TuneError::LayoutMismatch);
            }
            if spec.shape.is_numeric() != matches!(other_values, Values::Numbers(_)) {
                return Err(TuneError::LayoutMismatch);
            }
            values.push(other_values.clone());
        }
        self.values = values;
        self.edits.clear();
        Ok(())
    }

    /// Copies the named variables' values from `other`, leaving the rest.
    ///
    /// Used when a page was rebuilt and only some of its variables should move:
    /// the values a file carried, and the alias views that follow them.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError::LayoutMismatch`] when the tunes were built from
    /// different layouts, and [`TuneError::UnknownVariable`] when a name is not
    /// in the layout.
    pub fn adopt_named(&mut self, other: &Self, names: &[&str]) -> Result<(), TuneError> {
        if !self.same_layout(other) {
            return Err(TuneError::LayoutMismatch);
        }

        for name in names {
            let Some(spec) = self.layout.variable(name) else {
                return Err(TuneError::UnknownVariable((*name).to_owned()));
            };
            let Some(values) = other.value(&spec.name) else {
                return Err(TuneError::UnknownVariable(spec.name.clone()));
            };
            let copied = spec.name.clone();

            match values {
                Values::Numbers(numbers) => {
                    for (index, raw) in numbers.iter().enumerate() {
                        self.set_image_raw(&copied, index, *raw)?;
                    }
                }
                Values::Text(text) => {
                    self.set_image_text(&copied, text)?;
                }
            }
        }

        Ok(())
    }

    /// A copy of this tune's values without edit timestamps.
    ///
    /// This is what becomes the baseline image: "as persisted" is about values,
    /// not about when a cell was last typed into.
    #[must_use]
    pub fn snapshot(&self) -> Self {
        Self {
            layout: Arc::clone(&self.layout),
            values: self.values.clone(),
            edits: BTreeMap::new(),
        }
    }

    /// Every value vector, in layout order.
    pub fn values(&self) -> &[Values] {
        &self.values
    }

    /// One variable's values.
    pub fn value(&self, name: &str) -> Option<&Values> {
        let index = self.layout.index(name)?;
        self.values.get(index)
    }

    /// The raw value of a scalar or bit field.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError`] when the variable is unknown, is not a scalar, or
    /// is not numeric.
    pub fn scalar_raw(&self, name: &str) -> Result<f64, TuneError> {
        let variable = self.variable(name)?;
        if !variable.shape.is_scalar() {
            return Err(TuneError::WrongKind {
                name: name.to_owned(),
                expected: "scalar",
            });
        }
        self.raw_at(name, 0)
    }

    /// The user value of a scalar or bit field.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError`] when the variable is unknown, is not a scalar, or
    /// has an attribute that cannot be evaluated.
    pub fn scalar_user(&self, name: &str) -> Result<f64, TuneError> {
        let raw = self.scalar_raw(name)?;
        Ok(self.variable(name)?.to_user(raw)?)
    }

    /// The raw value at a flat index (arrays included).
    ///
    /// # Errors
    ///
    /// Returns [`TuneError`] when the variable is unknown, not numeric, or the
    /// index is outside the value.
    pub fn raw_at(&self, name: &str, index: usize) -> Result<f64, TuneError> {
        let values = self.value_at(name)?;
        match values {
            Values::Numbers(numbers) => {
                numbers
                    .get(index)
                    .copied()
                    .ok_or_else(|| TuneError::IndexOutOfRange {
                        name: name.to_owned(),
                        index,
                    })
            }
            Values::Text(_) => Err(TuneError::WrongKind {
                name: name.to_owned(),
                expected: "number",
            }),
        }
    }

    /// The user value at a flat index.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError`] when the variable is unknown, not numeric, the
    /// index is out of range, or a conversion attribute is unresolved.
    pub fn user_at(&self, name: &str, index: usize) -> Result<f64, TuneError> {
        let raw = self.raw_at(name, index)?;
        Ok(self.variable(name)?.to_user(raw)?)
    }

    /// The raw value at a table cell.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError`] when the variable is unknown, the cell is outside
    /// the shape, or the value is not numeric.
    pub fn cell_raw(&self, name: &str, row: usize, column: usize) -> Result<f64, TuneError> {
        let index = self.offset(name, row, column)?;
        self.raw_at(name, index)
    }

    /// The user value at a table cell.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError`] when the variable is unknown, the cell is outside
    /// the shape, or a conversion attribute is unresolved.
    pub fn cell_user(&self, name: &str, row: usize, column: usize) -> Result<f64, TuneError> {
        let index = self.offset(name, row, column)?;
        self.user_at(name, index)
    }

    /// The text of a string variable.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError`] when the variable is unknown or stores numbers.
    pub fn text(&self, name: &str) -> Result<&str, TuneError> {
        let variable = self.variable(name)?;
        if variable.shape.is_numeric() {
            return Err(TuneError::WrongKind {
                name: name.to_owned(),
                expected: "text",
            });
        }
        match self.value_at(name)? {
            Values::Text(text) => Ok(text),
            Values::Numbers(_) => unreachable!("shape and storage disagree"),
        }
    }

    /// Stores a raw value at a flat index.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError`] when the variable is unknown, not numeric, or the
    /// index is outside the value.
    pub fn set_raw_at(&mut self, name: &str, index: usize, raw: f64) -> Result<(), TuneError> {
        self.set_raw_at_time(name, index, raw, SystemTime::now())
    }

    fn set_raw_at_time(
        &mut self,
        name: &str,
        index: usize,
        raw: f64,
        at: SystemTime,
    ) -> Result<(), TuneError> {
        let variable = self.index_of(name)?;
        let len = self.values[variable].len();
        if index >= len {
            return Err(TuneError::IndexOutOfRange {
                name: name.to_owned(),
                index,
            });
        }
        match &mut self.values[variable] {
            Values::Numbers(numbers) => {
                numbers[index] = raw;
            }
            Values::Text(_) => {
                return Err(TuneError::WrongKind {
                    name: name.to_owned(),
                    expected: "number",
                });
            }
        }
        self.edits.insert(Cell { variable, index }, at);
        Ok(())
    }

    /// Stores a raw value from a page image, without recording an edit.
    ///
    /// Image loads (an ECU read-back, a snapshot restored from a file) are not
    /// user edits: marking them would make the whole tune look typed into.
    /// Only [`crate::decode_page`] and the image layers call this.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError`] when the variable is unknown, not numeric, or the
    /// index is outside the value.
    pub fn set_image_raw(&mut self, name: &str, index: usize, raw: f64) -> Result<(), TuneError> {
        let variable = self.index_of(name)?;
        let len = self.values[variable].len();
        if index >= len {
            return Err(TuneError::IndexOutOfRange {
                name: name.to_owned(),
                index,
            });
        }
        match &mut self.values[variable] {
            Values::Numbers(numbers) => {
                numbers[index] = raw;
                Ok(())
            }
            Values::Text(_) => Err(TuneError::WrongKind {
                name: name.to_owned(),
                expected: "number",
            }),
        }
    }

    /// Converts a user value and stores its raw form.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError`] when the variable is unknown, the index is out of
    /// range, or a conversion attribute is unresolved.
    pub fn set_user_at(
        &mut self,
        name: &str,
        index: usize,
        user: f64,
    ) -> Result<EditOutcome, TuneError> {
        self.set_user_at_time(name, index, user, SystemTime::now())
    }

    /// Stores a user value, recording `at` as the edit time.
    ///
    /// Timestamps are caller-supplied so tests are deterministic; the UI-facing
    /// [`Self::set_user_at`] uses the system clock.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError`] when the variable is unknown, the index is out of
    /// range, or a conversion attribute is unresolved.
    pub fn set_user_at_time(
        &mut self,
        name: &str,
        index: usize,
        user: f64,
        at: SystemTime,
    ) -> Result<EditOutcome, TuneError> {
        let variable = self.variable(name)?;
        let conversion = variable.to_raw(user)?;
        self.set_raw_at_time(name, index, conversion.raw, at)?;
        Ok(conversion.into())
    }

    /// Stores a user value in a scalar or bit field.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError`] when the variable is unknown, not a scalar, or a
    /// conversion attribute is unresolved.
    pub fn set_user(&mut self, name: &str, user: f64) -> Result<EditOutcome, TuneError> {
        let variable = self.variable(name)?;
        if !variable.shape.is_scalar() {
            return Err(TuneError::WrongKind {
                name: name.to_owned(),
                expected: "scalar",
            });
        }
        self.set_user_at(name, 0, user)
    }

    /// Stores a user value in a table cell.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError`] when the variable is unknown, the cell is outside
    /// the shape, or a conversion attribute is unresolved.
    pub fn set_cell_user(
        &mut self,
        name: &str,
        row: usize,
        column: usize,
        user: f64,
    ) -> Result<EditOutcome, TuneError> {
        let index = self.offset(name, row, column)?;
        self.set_user_at(name, index, user)
    }

    /// Replaces a string variable's text.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError`] when the variable is unknown or stores numbers.
    pub fn set_text(&mut self, name: &str, text: &str) -> Result<(), TuneError> {
        let index = self.index_of(name)?;
        self.set_image_text(name, text)?;
        self.edits.insert(
            Cell {
                variable: index,
                index: 0,
            },
            SystemTime::now(),
        );
        Ok(())
    }

    /// Replaces a string variable's text without recording an edit.
    ///
    /// A value that arrived from a file or a page is not a user edit, so it
    /// must not claim a slot in the edit log. Dirty state is a value diff and
    /// does not read that log, but a later reader of the log would.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError`] when the variable is unknown or stores numbers.
    pub fn set_image_text(&mut self, name: &str, text: &str) -> Result<(), TuneError> {
        let index = self.index_of(name)?;
        match &mut self.values[index] {
            Values::Text(current) => {
                text.clone_into(current);
            }
            Values::Numbers(_) => {
                return Err(TuneError::WrongKind {
                    name: name.to_owned(),
                    expected: "text",
                });
            }
        }
        Ok(())
    }

    /// When a cell was last edited through this tune, if it was.
    pub fn last_edit(&self, name: &str, row: usize, column: usize) -> Option<SystemTime> {
        let variable = self.layout.index(name)?;
        let index = self.layout.variables()[variable]
            .shape
            .offset(row, column)?;
        self.edits.get(&Cell { variable, index }).copied()
    }

    /// Whether the variable can be edited and will be sent.
    ///
    /// A controller-owned value and a `noMsqSave` value are refused because
    /// the write plan and the tune file both exclude them.
    pub fn is_writable(&self, name: &str) -> bool {
        self.layout
            .variable(name)
            .is_some_and(|variable| !variable.is_controller_owned() && !variable.no_msq_save)
            && !self.layout.is_alias(name)
    }

    /// One variable by name, with the normalised error for a miss.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError::UnknownVariable`] when the layout has no variable
    /// with this name.
    pub fn variable(&self, name: &str) -> Result<&VariableSpec, TuneError> {
        self.layout
            .variable(name)
            .ok_or_else(|| TuneError::UnknownVariable(name.to_owned()))
    }

    fn index_of(&self, name: &str) -> Result<usize, TuneError> {
        self.layout
            .index(name)
            .ok_or_else(|| TuneError::UnknownVariable(name.to_owned()))
    }

    fn value_at(&self, name: &str) -> Result<&Values, TuneError> {
        let index = self.index_of(name)?;
        Ok(&self.values[index])
    }

    fn offset(&self, name: &str, row: usize, column: usize) -> Result<usize, TuneError> {
        let variable = self.variable(name)?;
        variable
            .shape
            .offset(row, column)
            .ok_or_else(|| TuneError::CellOutOfRange {
                name: name.to_owned(),
                row,
                column,
            })
    }
}

fn default_values(variable: &VariableSpec) -> Values {
    let default = variable.default_raw;
    match variable.shape {
        EntryShape::Text { .. } => Values::Text(String::new()),
        EntryShape::Scalar | EntryShape::Bits { .. } => Values::numbers(default, 1),
        EntryShape::Array { .. } => Values::numbers(default, variable.shape.element_count()),
    }
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "unit tests may unwrap; a failed call is a failed test"
    )]
    use std::time::{Duration, UNIX_EPOCH};

    use oxtuner_ini::{SettingsContext, parse_str};

    use super::*;

    fn definition() -> Definition {
        parse_str(
            r#"
[MegaTune]
signature = "speeduino 202501"
queryCommand = "Q"

[Constants]
nPages = 1
pageSize = 288
page = 1
reqFuel = scalar, U16, 0, "ms", 0.1, 0, 0, 25.5, 1
veTable = array, U08, 32, [4x2], "%", 1, 0, 0, 255, 0
note = string, ASCII, 8, 4
"#,
            &SettingsContext::new(),
        )
        .expect("parses")
    }

    #[test]
    fn a_tune_starts_at_the_definition_defaults() {
        let tune = Tune::from_definition(&definition());

        assert_eq!(tune.scalar_raw("reqFuel").unwrap(), 0.0);
        assert_eq!(tune.text("note").unwrap(), "");
        assert_eq!(tune.value("veTable").unwrap().len(), 8);
    }

    #[test]
    fn a_cell_is_addressed_row_major() {
        let mut tune = Tune::from_definition(&definition());

        tune.set_cell_user("veTable", 1, 2, 42.0).unwrap();
        assert_eq!(tune.cell_user("veTable", 1, 2).unwrap(), 42.0);
        assert_eq!(tune.user_at("veTable", 6).unwrap(), 42.0);
        assert!(tune.cell_user("veTable", 2, 0).is_err());
    }

    #[test]
    fn conversions_round_trip_without_losing_the_raw_value() {
        let mut tune = Tune::from_definition(&definition());
        tune.set_user("reqFuel", 12.3).unwrap();

        assert_eq!(tune.scalar_raw("reqFuel").unwrap(), 123.0);
        assert!((tune.scalar_user("reqFuel").unwrap() - 12.3).abs() < 1e-12);
    }

    #[test]
    fn an_edit_records_its_timestamp() {
        let mut tune = Tune::from_definition(&definition());
        let at = UNIX_EPOCH + Duration::from_secs(1_000);

        tune.set_user_at_time("reqFuel", 0, 1.0, at).unwrap();

        assert_eq!(tune.last_edit("reqFuel", 0, 0), Some(at));
        assert_eq!(tune.last_edit("veTable", 0, 0), None);
    }

    #[test]
    fn a_snapshot_keeps_values_and_drops_edit_times() {
        let mut tune = Tune::from_definition(&definition());
        tune.set_user("reqFuel", 5.0).unwrap();
        let at = UNIX_EPOCH;
        tune.set_user_at_time("reqFuel", 0, 7.0, at).unwrap();

        let snapshot = tune.snapshot();

        assert_eq!(snapshot.scalar_raw("reqFuel").unwrap(), 70.0);
        assert_eq!(snapshot.last_edit("reqFuel", 0, 0), None);
        assert!(tune.last_edit("reqFuel", 0, 0).is_some());
    }

    #[test]
    fn a_matching_import_replaces_values_and_drops_edit_times() {
        let definition = definition();
        let mut target = Tune::from_definition(&definition);
        target
            .set_user_at_time("reqFuel", 0, 9.0, UNIX_EPOCH)
            .expect("edit is recorded");
        let mut imported = Tune::from_definition(&definition);
        imported.set_user("reqFuel", 3.0).expect("value is set");

        target.adopt_values(&imported).expect("layouts match");

        assert_eq!(target.scalar_raw("reqFuel").expect("imported"), 30.0);
        assert_eq!(target.last_edit("reqFuel", 0, 0), None);
    }

    #[test]
    fn adopting_numbers_onto_a_text_variable_is_refused() {
        let definition = definition();
        let mut target = Tune::from_definition(&definition);
        let mut imported = Tune::from_definition(&definition);

        // One element passes the length check, so only the kind check can
        // refuse it.
        let note = imported
            .layout()
            .index("note")
            .expect("note is in the layout");
        imported.values[note] = Values::Numbers(vec![1.0]);

        assert_eq!(
            target.adopt_values(&imported),
            Err(TuneError::LayoutMismatch),
        );
        assert_eq!(target.text("note").expect("text is unchanged"), "");
    }

    #[test]
    fn adopting_text_onto_a_numeric_variable_is_refused() {
        let definition = definition();
        let mut target = Tune::from_definition(&definition);
        let mut imported = Tune::from_definition(&definition);

        let req_fuel = imported
            .layout()
            .index("reqFuel")
            .expect("reqFuel is in the layout");
        imported.values[req_fuel] = Values::Text("nope".to_owned());

        assert_eq!(
            target.adopt_values(&imported),
            Err(TuneError::LayoutMismatch),
        );
        assert_eq!(target.scalar_raw("reqFuel").expect("unchanged"), 0.0);
    }

    #[test]
    fn an_unknown_name_is_an_error_not_a_default() {
        let mut tune = Tune::from_definition(&definition());

        assert_eq!(
            tune.scalar_raw("missing"),
            Err(TuneError::UnknownVariable("missing".to_owned()))
        );
        assert_eq!(
            tune.set_user("missing", 1.0),
            Err(TuneError::UnknownVariable("missing".to_owned()))
        );
    }

    #[test]
    fn text_and_numeric_values_do_not_mix() {
        let mut tune = Tune::from_definition(&definition());

        assert!(matches!(
            tune.scalar_raw("note"),
            Err(TuneError::WrongKind {
                expected: "scalar",
                ..
            })
        ));
        assert!(matches!(
            tune.text("reqFuel"),
            Err(TuneError::WrongKind {
                expected: "text",
                ..
            })
        ));

        tune.set_text("note", "ME7.5").unwrap();
        assert_eq!(tune.text("note").unwrap(), "ME7.5");
    }
}
