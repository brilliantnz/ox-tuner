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

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::SystemTime;

use oxtuner_ini::Definition;

use crate::{
    AttributeError, EntryShape, IniPage, RawConversion, TuneLayout, ValueConflict, Values,
    VariableSpec,
};

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
    /// Two variables sharing bytes hold values the bytes cannot both carry.
    ConflictingValues {
        /// The pair and the bytes they share.
        conflict: ValueConflict,
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
            Self::ConflictingValues { conflict } => {
                write!(formatter, "{conflict}, and one value cannot carry both")
            }
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

    /// Re-derives every value that shares bytes with `name` from the page image.
    ///
    /// Two constants can sit over the same bytes, and editing one of them moves
    /// bytes the other also claims. Left alone the two disagree, and a write
    /// would settle them in declaration order. So the edited value is written
    /// to the page image and every constant that shares its bytes is read back
    /// from it, the same way a load reconciles a page. The edited value is the
    /// one that survives, and the others follow the bytes.
    ///
    /// A view is handled the same way. It is never written on its own, but
    /// patching the page with it moves the bytes its owner claims, which is
    /// what a file that carries a view without its owner already does.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError`] when the page cannot be encoded, patched or
    /// decoded, or when a value's shape does not fit the layout.
    pub fn reconcile_shared_bytes(&mut self, name: &str) -> Result<(), TuneError> {
        let layout = Arc::clone(&self.layout);
        if layout.variable(name).is_none() {
            return Ok(());
        }
        let Some(page) = layout.variable(name).and_then(VariableSpec::ini_page) else {
            return Ok(());
        };

        let base = crate::wire::encode_page(&layout, page, self)?;
        let patched = crate::wire::patch_page(&layout, page, &base, self, &[name])?;

        let mut decoded = self.snapshot();
        crate::wire::decode_page(&layout, page, &patched, &mut decoded)?;

        // A peer with a tighter range cannot store what the edited value leaves
        // in the bytes. Sending that would put a value on the wire its own clamp
        // rejects, so the re-derivation stands down and the write gate refuses
        // the disagreement instead. A word and a bit field inside it hold
        // different numbers and are not judged against each other.
        let storable = self.same_value_peers(name).iter().all(|peer| {
            let Some(spec) = layout.variable(peer) else {
                return true;
            };

            (0..spec.element_count()).all(|cell| {
                decoded
                    .raw_at(peer, cell)
                    .is_ok_and(|value| spec.same_wire_value(spec.clamp_raw(value), value))
            })
        });
        if !storable {
            return Ok(());
        }

        let moved = layout.names_sharing_bytes(name);
        self.adopt_named(&decoded, &moved)
    }

    /// The variables storing the same quantity as `name` over the same bytes.
    ///
    /// Two constants are peers here only when they start at the same address
    /// and hold the same kind of value, which is what makes one number of them
    /// comparable. A word and a bit field inside it share bytes but hold
    /// different numbers, each read against its own range, and a text value
    /// holds none of either, so neither takes part.
    fn same_value_peers(&self, name: &str) -> Vec<String> {
        let Some(spec) = self.layout.variable(name) else {
            return Vec::new();
        };
        if !spec.shape.is_numeric() {
            return Vec::new();
        }

        self.layout
            .names_sharing_bytes(name)
            .into_iter()
            .filter(|peer| {
                self.layout.variable(peer).is_some_and(|other| {
                    other.address == spec.address
                        && other.data_type == spec.data_type
                        && other.shape == spec.shape
                })
            })
            .map(str::to_owned)
            .collect()
    }

    /// Makes every value that shares bytes agree on one they can all store.
    ///
    /// Two constants over the same bytes can declare different ranges, and a
    /// clamp moves each of them on its own. That leaves a pair holding two
    /// values for one byte even where the caller stated one, and the write gate
    /// then refuses the page over something nobody asked for. So a group that
    /// already holds one value is pulled onto the tightest range among its
    /// members and that value is written back to all of them. A group that
    /// holds two different values is left alone: that disagreement is real and
    /// judging it belongs to the caller. So is a group whose ranges have no
    /// value in common, because the fold would then put a value one of them
    /// rejects on the tune and hide the disagreement from the write gate.
    pub fn clamp_shared_bytes(&mut self) {
        let mut settled: HashSet<String> = HashSet::new();

        for page in self.layout.pages().to_vec() {
            let names: Vec<String> = page
                .variables
                .iter()
                .copied()
                .filter_map(|index| self.layout.variables().get(index))
                .map(|spec| spec.name.clone())
                .collect();

            for name in names {
                if settled.contains(&name) {
                    continue;
                }

                let peers = self.same_value_peers(&name);
                for peer in &peers {
                    settled.insert(peer.clone());
                }
                if peers.len() < 2 {
                    continue;
                }
                let Some(spec) = self.layout.variable(&name) else {
                    continue;
                };
                let cells = spec.element_count();

                for cell in 0..cells {
                    let Ok(value) = self.raw_at(&name, cell) else {
                        continue;
                    };
                    let agreed = peers.iter().all(|peer| {
                        self.raw_at(peer, cell).is_ok_and(|raw| {
                            self.layout
                                .variable(peer)
                                .is_some_and(|other| other.same_wire_value(raw, value))
                        })
                    });
                    if !agreed {
                        continue;
                    }

                    let common = peers.iter().fold(value, |acc, peer| {
                        self.layout
                            .variable(peer)
                            .map_or(acc, |other| other.clamp_raw(acc))
                    });
                    // Two ranges can have no value in common at all, and the
                    // fold then lands somewhere one of them rejects. Writing it
                    // would leave the tune holding something its own clamp
                    // turns down and hide the disagreement from the write gate.
                    let storable = peers.iter().all(|peer| {
                        self.layout.variable(peer).is_some_and(|other| {
                            other.same_wire_value(other.clamp_raw(common), common)
                        })
                    });
                    if !storable {
                        continue;
                    }

                    for peer in &peers {
                        let _ = self.set_image_raw(peer, cell, common);
                    }
                }
            }
        }
    }

    /// Makes every value that shares bytes agree, page by page.
    ///
    /// Where a definition gives two constants the same bytes and its own
    /// defaults disagree, there is no user intent to respect yet and one value
    /// still has to be picked. This writes each page image and reads it back,
    /// which is the settle the codec does anyway: the page codec runs non-bits
    /// first and bits after, so among peers of the same kind the later declared
    /// one keeps its value while a `bits` field always merges last. On a page it
    /// rebuilds, every constant follows the result, views included, and each
    /// group is then pulled onto a value all of it can store, so the tune is
    /// consistent afterwards instead of starting out with a pair no write would
    /// carry.
    ///
    /// A page [`TuneLayout::page_can_be_rebuilt`] refuses is never written and
    /// read back, because constants whose bytes overlap without one covering
    /// the other have no owner and the codec would settle them in declaration
    /// order and drop one of them, while a text field overlapping a number
    /// would be read back as part of that text. A view on one still moves,
    /// since it is not a value of its own and follows the bytes its owner
    /// holds, and the clamp below still runs over the pairs the page's bytes
    /// cover.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError`] when a page cannot be encoded or decoded, when a
    /// value's shape does not fit the layout, or when a view cannot be derived
    /// from the variable that owns its bytes.
    pub fn settle_shared_bytes(&mut self) -> Result<(), TuneError> {
        let layout = Arc::clone(&self.layout);

        for page in layout.pages() {
            if !layout.page_can_be_rebuilt(page.page) {
                layout.derive_views(page.page, self, &[])?;
                continue;
            }

            let names: Vec<&str> = page
                .variables
                .iter()
                .copied()
                .filter_map(|index| layout.variables().get(index))
                .map(|spec| spec.name.as_str())
                .collect();
            if names.is_empty() {
                continue;
            }

            let bytes = crate::wire::encode_page(&layout, page.page, self)?;
            let mut decoded = self.snapshot();
            crate::wire::decode_page(&layout, page.page, &bytes, &mut decoded)?;

            self.adopt_named(&decoded, &names)?;
        }
        self.clamp_shared_bytes();

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
    /// A controller-owned value, a `noMsqSave` value and one whose scale or
    /// translate is unresolved are refused: none can reach the wire or the
    /// file, so an edit would be lost.
    pub fn is_writable(&self, name: &str) -> bool {
        self.layout.variable(name).is_some_and(|variable| {
            !variable.is_controller_owned()
                && !variable.no_msq_save
                && variable.scale.is_some()
                && variable.translate.is_some()
        }) && !self.layout.is_alias(name)
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

    /// A word holding a bit field, and an array holding a view of its first cell.
    const SHARED: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 16
page = 1
word = scalar, U08, 0, "bits", 1, 0
fan = bits, U08, 0, [0:1]
data = array, U08, 4, [4], "", 1, 0, 0, 255, 0
data0 = bits, U08, 4, [0:7]
"#;

    fn shared() -> Tune {
        let definition = parse_str(SHARED, &SettingsContext::new()).expect("parses");
        Tune::from_definition(&definition)
    }

    #[test]
    fn an_edited_bit_field_keeps_the_rest_of_its_word() {
        let mut tune = shared();
        // 0b1010_0101 carries 1 in bits 0..1 and other bits besides.
        tune.set_image_raw("word", 0, 165.0).unwrap();
        tune.reconcile_shared_bytes("word").unwrap();
        assert_eq!(tune.raw_at("fan", 0).unwrap(), 1.0);
        assert_eq!(tune.raw_at("word", 0).unwrap(), 165.0);

        // Moving the field moves its own bits and nothing else: the value
        // named to the re-derivation is the one that survives.
        tune.set_image_raw("fan", 0, 2.0).unwrap();
        tune.reconcile_shared_bytes("fan").unwrap();
        assert_eq!(tune.raw_at("fan", 0).unwrap(), 2.0);
        assert_eq!(tune.raw_at("word", 0).unwrap(), 166.0, "0b1010_0110");
    }

    #[test]
    fn an_edited_word_moves_the_bit_field_that_sits_in_it() {
        let mut tune = shared();

        tune.set_image_raw("word", 0, 3.0).unwrap();
        tune.reconcile_shared_bytes("word").unwrap();
        assert_eq!(
            tune.raw_at("fan", 0).unwrap(),
            3.0,
            "the field reads the bits the word now carries"
        );

        tune.set_image_raw("word", 0, 0.0).unwrap();
        tune.reconcile_shared_bytes("word").unwrap();
        assert_eq!(tune.raw_at("fan", 0).unwrap(), 0.0);
    }

    #[test]
    fn an_edited_view_moves_the_bytes_its_owner_holds() {
        let mut tune = shared();
        for (index, value) in [1.0, 2.0, 3.0, 4.0].into_iter().enumerate() {
            tune.set_image_raw("data", index, value).unwrap();
        }

        tune.set_image_raw("data0", 0, 9.0).unwrap();
        tune.reconcile_shared_bytes("data0").unwrap();

        assert_eq!(
            tune.raw_at("data", 0).unwrap(),
            9.0,
            "a view is never written on its own, but it moves the owner's bytes"
        );
        assert_eq!(tune.raw_at("data0", 0).unwrap(), 9.0);
        assert_eq!(
            tune.raw_at("data", 1).unwrap(),
            2.0,
            "the elements the view does not cover are untouched"
        );
    }

    #[test]
    fn settling_shared_bytes_leaves_one_value_for_both() {
        // A definition can disagree with itself about what a shared byte holds:
        // `second` cannot read back as zero inside its range, so its default is
        // seven while `first` stays at zero.
        let definition = parse_str(
            r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 8
page = 1
first = scalar, U08, 0, "x", 1, 0, 0, 255, 0
second = scalar, U08, 0, "x", 1, 0, 7, 255, 0
"#,
            &SettingsContext::new(),
        )
        .expect("parses");
        let mut tune = Tune::from_definition(&definition);
        let layout = Arc::clone(tune.layout());

        assert_eq!(tune.raw_at("first", 0).unwrap(), 0.0);
        assert_eq!(tune.raw_at("second", 0).unwrap(), 7.0);
        assert_eq!(layout.value_conflicts(IniPage::FIRST, &tune).len(), 1);

        tune.settle_shared_bytes().unwrap();

        assert!(
            layout.value_conflicts(IniPage::FIRST, &tune).is_empty(),
            "one byte holds one value: {:?}",
            tune.raw_at("first", 0)
        );
        assert_eq!(
            tune.raw_at("first", 0).unwrap(),
            tune.raw_at("second", 0).unwrap()
        );
    }

    #[test]
    fn a_peer_that_cannot_store_the_survivor_is_left_disagreeing() {
        // `tight` cannot store what `loose` can, so the re-derivation stands
        // down rather than move `tight` onto a value its own clamp rejects.
        // The write gate still sees the pair and refuses the page.
        let definition = parse_str(
            r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 8
page = 1
loose = scalar, U08, 0, "x", 1, 0, 0, 255, 0
tight = scalar, U08, 0, "x", 1, 0, 0, 5, 0
"#,
            &SettingsContext::new(),
        )
        .expect("parses");
        let mut tune = Tune::from_definition(&definition);

        tune.set_image_raw("loose", 0, 12.0).unwrap();
        tune.set_image_raw("tight", 0, 3.0).unwrap();
        tune.reconcile_shared_bytes("loose").unwrap();

        assert_eq!(
            tune.raw_at("tight", 0).unwrap(),
            3.0,
            "the peer keeps its own value rather than taking one its clamp rejects"
        );
        assert_eq!(
            tune.raw_at("loose", 0).unwrap(),
            12.0,
            "nothing moved at all"
        );
        assert_eq!(
            tune.layout().value_conflicts(IniPage::FIRST, &tune).len(),
            1,
            "the gate still sees the disagreement"
        );
    }

    /// Peers over one byte whose declared ranges do not reach each other.
    const NARROW: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 8
page = 1
first = scalar, U08, 0, "x", 1, 0, 0, 255, 0
second = scalar, U08, 0, "x", 1, 0, 0, 5, 0
"#;

    fn narrow() -> Tune {
        let definition = parse_str(NARROW, &SettingsContext::new()).expect("parses");
        Tune::from_definition(&definition)
    }

    #[test]
    fn a_group_that_agrees_is_pulled_onto_the_tightest_range() {
        let mut tune = narrow();
        tune.set_image_raw("first", 0, 12.0).unwrap();
        tune.set_image_raw("second", 0, 12.0).unwrap();

        tune.clamp_shared_bytes();

        assert_eq!(tune.raw_at("first", 0).unwrap(), 5.0);
        assert_eq!(tune.raw_at("second", 0).unwrap(), 5.0);
        assert!(
            tune.layout()
                .value_conflicts(IniPage::FIRST, &tune)
                .is_empty(),
            "one byte holds one value now"
        );
    }

    #[test]
    fn a_group_that_disagrees_is_left_alone() {
        let mut tune = narrow();
        tune.set_image_raw("first", 0, 12.0).unwrap();
        tune.set_image_raw("second", 0, 3.0).unwrap();

        tune.clamp_shared_bytes();

        assert_eq!(tune.raw_at("first", 0).unwrap(), 12.0);
        assert_eq!(tune.raw_at("second", 0).unwrap(), 3.0);
        assert_eq!(
            tune.layout().value_conflicts(IniPage::FIRST, &tune).len(),
            1,
            "a real disagreement belongs to the caller, not to a clamp"
        );
    }

    #[test]
    fn a_group_with_no_common_value_keeps_its_values() {
        let definition = parse_str(
            r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 8
page = 1
first = scalar, U08, 0, "x", 1, 0, 0, 10, 0
second = scalar, U08, 0, "x", 1, 0, 20, 255, 0
"#,
            &SettingsContext::new(),
        )
        .expect("parses");
        let mut tune = Tune::from_definition(&definition);

        // Both hold fifteen, which no member of the pair can store: `first`
        // tops out at ten and `second` starts at twenty. Nothing may move,
        // because a value either one would reject must not land on the tune and
        // hide the disagreement from the write gate.
        tune.set_image_raw("first", 0, 15.0).unwrap();
        tune.set_image_raw("second", 0, 15.0).unwrap();
        tune.clamp_shared_bytes();

        assert_eq!(tune.raw_at("first", 0).unwrap(), 15.0);
        assert_eq!(tune.raw_at("second", 0).unwrap(), 15.0);
    }

    #[test]
    fn settling_leaves_a_page_with_an_ambiguous_pair_alone() {
        // `wide` runs 0..4 and `straddle` runs 2..6, so neither covers the
        // other and no value owns what they share. Rebuilding the page would
        // fill those bytes in declaration order and silently rewrite the tail
        // of `wide`, exactly what a load refuses to do.
        let definition = parse_str(
            r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 16
page = 1
wide = array, U08, 0, [4], "", 1, 0, 0, 255, 0
straddle = array, U08, 2, [4], "", 1, 0, 7, 255, 0
"#,
            &SettingsContext::new(),
        )
        .expect("parses");
        let mut tune = Tune::from_definition(&definition);

        tune.settle_shared_bytes().unwrap();

        for cell in 0..4 {
            assert_eq!(tune.raw_at("wide", cell).unwrap(), 0.0);
            assert_eq!(
                tune.raw_at("straddle", cell).unwrap(),
                7.0,
                "cell {cell} of the constant the rebuild would have written over"
            );
        }
    }

    #[test]
    fn settling_leaves_a_page_with_a_text_overlap_alone() {
        // The codec reads a text field as its whole length, so a number under
        // one comes back as part of the text. Rebuilding the page would turn
        // `note` into whatever bytes `inside` writes into its middle.
        let definition = parse_str(
            r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 16
page = 1
note = string, ASCII, 8
inside = scalar, U08, 4, "x", 1, 0, 7, 255, 0
"#,
            &SettingsContext::new(),
        )
        .expect("parses");
        let mut tune = Tune::from_definition(&definition);

        tune.settle_shared_bytes().unwrap();

        assert_eq!(tune.text("note").unwrap(), "");
        assert_eq!(tune.raw_at("inside", 0).unwrap(), 7.0);
    }

    #[test]
    fn settling_skips_one_page_without_skipping_the_next() {
        // Page 1 cannot be rebuilt and page 2 can. Skipping must not mean
        // giving up on the pages after it.
        let definition = parse_str(
            r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 2
pageSize = 16, 16
page = 1
wide = array, U08, 0, [4], "", 1, 0, 0, 255, 0
straddle = array, U08, 2, [4], "", 1, 0, 7, 255, 0
page = 2
first = scalar, U08, 0, "x", 1, 0, 0, 255, 0
second = scalar, U08, 0, "x", 1, 0, 7, 255, 0
"#,
            &SettingsContext::new(),
        )
        .expect("parses");
        let mut tune = Tune::from_definition(&definition);

        tune.settle_shared_bytes().unwrap();

        assert_eq!(
            tune.raw_at("first", 0).unwrap(),
            tune.raw_at("second", 0).unwrap(),
            "the peers on the page that can be rebuilt still settle"
        );
        assert_eq!(tune.raw_at("wide", 3).unwrap(), 0.0);
        assert_eq!(
            tune.raw_at("straddle", 0).unwrap(),
            7.0,
            "and the page that cannot be is still untouched"
        );
    }

    #[test]
    fn clamping_still_runs_on_a_page_settling_skips() {
        // The pair under `first`/`second` is legal and agrees on twelve, which
        // only `first` can store. The clamp has to pull both onto five even
        // though the page is one the rebuild skipped.
        let definition = parse_str(
            r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 16
page = 1
wide = array, U08, 0, [4], "", 1, 0, 0, 255, 0
straddle = array, U08, 2, [4], "", 1, 0, 7, 255, 0
first = scalar, U08, 8, "x", 1, 0, 0, 255, 0
second = scalar, U08, 8, "x", 1, 0, 0, 5, 0
"#,
            &SettingsContext::new(),
        )
        .expect("parses");
        let mut tune = Tune::from_definition(&definition);
        tune.set_image_raw("first", 0, 12.0).unwrap();
        tune.set_image_raw("second", 0, 12.0).unwrap();

        tune.settle_shared_bytes().unwrap();

        assert_eq!(tune.raw_at("first", 0).unwrap(), 5.0);
        assert_eq!(tune.raw_at("second", 0).unwrap(), 5.0);
        assert_eq!(
            tune.raw_at("wide", 3).unwrap(),
            0.0,
            "a rebuild would put the straddle's seven into this cell"
        );
    }

    #[test]
    fn settling_derives_the_views_of_a_page_it_cannot_rebuild() {
        // `view` reads `owner`'s second byte, and `note` is a text field over
        // the owner's remaining bytes, so the page cannot go through the codec.
        // A view is still not a value of its own: it follows the bytes its
        // owner holds even on a page whose numbers and text are left standing.
        let definition = parse_str(
            r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 16
page = 1
owner = array, U08, 0, [4], "", 1, 0, 7, 255, 0
view = scalar, U08, 1, "", 1, 0, 0, 255, 0
note = string, ASCII, 8
"#,
            &SettingsContext::new(),
        )
        .expect("parses");
        let mut tune = Tune::from_definition(&definition);
        assert!(
            !tune.layout().page_can_be_rebuilt(IniPage::FIRST),
            "this is the derive fallback only when the page is refused"
        );
        for (index, value) in [1.0, 2.0, 3.0, 4.0].into_iter().enumerate() {
            tune.set_image_raw("owner", index, value).unwrap();
        }

        tune.settle_shared_bytes().unwrap();

        assert_eq!(
            tune.raw_at("view", 0).unwrap(),
            2.0,
            "the view reads its owner's second byte, not its own default"
        );
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
