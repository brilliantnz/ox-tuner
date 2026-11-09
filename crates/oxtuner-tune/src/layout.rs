//! The variable layout: what a [`Definition`] says a tune contains.
//!
//! A tune is not a free-form map of names to numbers. Every value is interpreted
//! through one frozen layout entry: its page, its shape, its storage type and
//! its presentation attributes. Building that layout once, up front, is what
//! keeps the rest of the crate cheap:
//!
//! - a value lookup is `O(1)` by name (ASCII-case-insensitive, like INI references);
//! - a table cell is `O(1)` by `row × cols + column`;
//! - a page is a pre-built list of variable indices, so write planning and
//!   state derivation never scan the whole tune.
//!
//! The layout is built from a [`Definition`] plus a lookup of **user-unit
//! values** for presentation attributes that are written as expressions
//! (`{0.1 / stoich}`). Those attributes re-evaluate at runtime in the dialect
//! (see [`oxtuner_ini::Number`]); M3 resolves them once, against the tune being
//! loaded, and reports any it cannot resolve instead of guessing. Re-evaluating
//! them live as the user edits is M4 work.

use std::collections::HashMap;
use std::fmt;

use oxtuner_ini::{ConstantClass, DataType, Definition, Endianness, Number, Shape};

use crate::IniPage;

/// FNV-1a over the ASCII-lowercased bytes.
///
/// The layout lookup is case-insensitive without allocating a lowercased
/// `String` per variable at build time or per value at load time. A collision
/// keeps the first index and makes `index` fall back to the linear scan, so
/// the result is the same as the old lowercased-string map.
fn ci_hash(name: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &byte in name.as_bytes() {
        hash ^= u64::from(byte.to_ascii_lowercase());
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The declared shape of one variable, with every dimension resolved.
#[expect(
    clippy::exhaustive_enums,
    reason = "the INI dialect defines a closed set of storage shapes"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryShape {
    /// One value.
    Scalar,
    /// A bit field inside a larger word.
    Bits {
        /// Lowest bit, inclusive.
        low: u8,
        /// Highest bit, inclusive.
        high: u8,
        /// Added to the presented value (`[0:2+1]`, the `+1`).
        shift: u8,
    },
    /// A 1-D or 2-D array, row-major. A list is `rows == 1`.
    Array {
        /// Number of columns.
        cols: usize,
        /// Number of rows.
        rows: usize,
    },
    /// Fixed-length text.
    Text {
        /// Number of characters the value holds.
        len: usize,
    },
}

impl EntryShape {
    /// Number of stored elements.
    ///
    /// Text is stored as one string cell even when the definition reserves
    /// `len` characters on the wire, so it counts as one here. Counting the
    /// reserved length would make the state and diff layers compare the same
    /// string `len` times.
    pub const fn element_count(self) -> usize {
        match self {
            Self::Scalar | Self::Bits { .. } | Self::Text { .. } => 1,
            Self::Array { cols, rows } => cols.saturating_mul(rows),
        }
    }

    /// Whether the shape holds numbers rather than text.
    pub const fn is_numeric(self) -> bool {
        !matches!(self, Self::Text { .. })
    }

    /// The row/column dimensions, when the shape is an array.
    pub const fn dimensions(self) -> Option<(usize, usize)> {
        match self {
            Self::Array { cols, rows } => Some((cols, rows)),
            _ => None,
        }
    }

    /// Whether a single value is addressed as a cell.
    pub const fn is_scalar(self) -> bool {
        matches!(self, Self::Scalar | Self::Bits { .. })
    }

    /// Offset of `(row, column)` inside the value vector, when in range.
    pub fn offset(self, row: usize, column: usize) -> Option<usize> {
        match self {
            Self::Scalar | Self::Bits { .. } | Self::Text { .. } => {
                (row == 0 && column == 0).then_some(0)
            }
            Self::Array { cols, rows } => {
                (row < rows && column < cols).then_some(row.saturating_mul(cols) + column)
            }
        }
    }
}

/// Who owns a variable's value.
#[expect(
    clippy::exhaustive_enums,
    reason = "a variable is either on an INI page or PC-side"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Owner {
    /// A calibration constant on an INI page (1-based).
    Page(IniPage),
    /// A PC-side variable: stored in the tune file, never sent to the ECU.
    Pc,
}

impl Owner {
    /// The INI page, when the variable is a calibration constant.
    pub const fn page(self) -> Option<IniPage> {
        match self {
            Self::Page(page) => Some(page),
            Self::Pc => None,
        }
    }
}

/// Everything the tune needs to know about one constant or PC variable.
///
/// Presentation attributes are resolved numbers here. `None` for
/// [`Self::scale`] or [`Self::translate`] means the INI wrote an expression
/// this build could not evaluate; conversions then fail loudly instead of
/// pretending the value is unscaled.
#[derive(Debug, Clone, PartialEq)]
pub struct VariableSpec {
    /// Name as written in the definition.
    pub name: String,
    /// Where the value lives.
    pub owner: Owner,
    /// Declared shape.
    pub shape: EntryShape,
    /// Wire storage type.
    pub data_type: DataType,
    /// Offset inside the page, when the definition resolved one.
    ///
    /// `None` means the address could not be resolved (an unknown expression
    /// or a malformed offset); such a variable cannot be taken from or written
    /// to a page image, and the page codec reports it rather than guessing.
    pub address: Option<u32>,
    /// Display units (empty when the definition writes a text expression).
    pub units: String,
    /// `user = (raw + translate) × scale`.
    pub scale: Option<f64>,
    /// `user = (raw + translate) × scale`.
    pub translate: Option<f64>,
    /// Minimum user value, when the definition states one.
    pub minimum: Option<f64>,
    /// Maximum user value, when the definition states one.
    pub maximum: Option<f64>,
    /// Decimal digits to display.
    pub digits: u8,
    /// The controller owns the value: the UI must not offer an edit.
    pub controller_owned: bool,
    /// `noMsqSave`: never written to a calibration file.
    pub no_msq_save: bool,
    /// Raw value used when the tune file has no entry for this variable.
    pub default_raw: f64,
}

impl VariableSpec {
    /// The page this variable is written to, if it is a constant.
    pub const fn ini_page(&self) -> Option<IniPage> {
        self.owner.page()
    }

    /// The value vector length this variable occupies.
    pub const fn element_count(&self) -> usize {
        self.shape.element_count()
    }

    /// Whether the definition marks the variable as controller-owned.
    pub const fn is_controller_owned(&self) -> bool {
        self.controller_owned
    }
}

/// One calibration page and the variables it carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageLayout {
    /// Page number as written in the INI (1-based).
    pub page: IniPage,
    /// Page size in bytes, from the definition.
    pub size: u32,
    /// Base address added to every constant offset (`parameterStartOffset`).
    pub start_offset: u32,
    /// Indices into [`TuneLayout::variables`], in definition order.
    pub variables: Vec<usize>,
}

impl PageLayout {
    /// Number of cells on the page.
    pub fn cell_count(&self, layout: &TuneLayout) -> usize {
        self.variables
            .iter()
            .map(|index| layout.variables[*index].element_count())
            .sum()
    }
}

/// How serious a layout problem is.
#[expect(
    clippy::exhaustive_enums,
    reason = "a layout problem either degrades a variable or drops it"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IssueSeverity {
    /// The variable loaded, but something about it is degraded.
    Warning,
    /// The variable could not be represented in the tune.
    Error,
}

/// Why a definition entry did not become a usable layout entry.
#[expect(
    clippy::exhaustive_enums,
    reason = "layout issues are a closed set of resolution failures"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutIssueKind {
    /// A constant was declared before any `page =` line.
    NoPage,
    /// An array or text dimension is an expression this build cannot fold.
    UnresolvedShape,
    /// A presentation attribute (`scale`, `translate`, …) is an expression
    /// that did not evaluate against the loaded values.
    UnresolvedAttribute,
    /// A constant names a page the definition never declared.
    UnknownPage,
}

/// One problem found while building the layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutIssue {
    /// How serious the problem is.
    pub severity: IssueSeverity,
    /// What kind of problem it is.
    pub kind: LayoutIssueKind,
    /// The variable involved, when there is one.
    pub name: Option<String>,
    /// A message ready to show in the output panel.
    pub message: String,
}

impl LayoutIssue {
    fn new(
        severity: IssueSeverity,
        kind: LayoutIssueKind,
        name: Option<&str>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity,
            kind,
            name: name.map(str::to_owned),
            message: message.into(),
        }
    }
}

impl fmt::Display for LayoutIssue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(name) = &self.name {
            write!(formatter, "`{name}`: ")?;
        }
        formatter.write_str(&self.message)
    }
}

/// The frozen shape of one tune.
#[derive(Debug, Clone)]
pub struct TuneLayout {
    signature: String,
    endianness: Endianness,
    variables: Vec<VariableSpec>,
    /// Hash of the folded name -> first variable index with that hash.
    lookup: HashMap<u64, usize>,
    pages: Vec<PageLayout>,
    issues: Vec<LayoutIssue>,
}

impl TuneLayout {
    /// Builds the layout from a definition.
    ///
    /// `values` supplies **user-unit** values for names referenced by
    /// presentation expressions; pass `&|_| None` when no tune is loaded yet.
    /// Missing references are reported in [`Self::issues`], never guessed.
    pub fn build(definition: &Definition, values: &dyn Fn(&str) -> Option<f64>) -> Self {
        let mut layout = Self {
            signature: definition.signature.clone(),
            endianness: definition.endianness,
            variables: Vec::new(),
            lookup: HashMap::new(),
            pages: Vec::new(),
            issues: Vec::new(),
        };

        for (name, constant) in definition.constants.iter() {
            layout.add_constant(definition, name, constant, values);
        }

        for (name, variable) in definition.pc_variables.iter() {
            layout.add_pc_variable(definition, name, variable, values);
        }

        layout.build_pages(definition);
        layout
    }

    fn add_constant(
        &mut self,
        definition: &Definition,
        name: &str,
        constant: &oxtuner_ini::Constant,
        values: &dyn Fn(&str) -> Option<f64>,
    ) {
        let Some(page) = IniPage::new(constant.page) else {
            self.issues.push(LayoutIssue::new(
                IssueSeverity::Error,
                LayoutIssueKind::NoPage,
                Some(name),
                "the constant is declared before any `page =` line, so it has no ECU address",
            ));
            return;
        };

        let Some(shape) = shape_of(&constant.shape) else {
            self.issues.push(LayoutIssue::new(
                IssueSeverity::Error,
                LayoutIssueKind::UnresolvedShape,
                Some(name),
                "the declared shape depends on a value this build cannot resolve, so the \
                 constant has no fixed size",
            ));
            return;
        };

        let attributes = Attributes::resolve(
            constant.scale.as_ref(),
            constant.translate.as_ref(),
            constant.minimum.as_ref(),
            constant.maximum.as_ref(),
            constant.digits.as_ref(),
            values,
        );
        if let Some(unresolved) = attributes.unresolved {
            self.issues.push(LayoutIssue::new(
                IssueSeverity::Warning,
                LayoutIssueKind::UnresolvedAttribute,
                Some(name),
                format!(
                    "presentation attribute `{unresolved}` is an expression this build could \
                     not evaluate; values cannot be converted until it resolves"
                ),
            ));
        }

        let controller_owned =
            constant.flags.controller_priority || constant.class == ConstantClass::OddArray;
        let spec = VariableSpec {
            name: name.to_owned(),
            owner: Owner::Page(page),
            shape,
            data_type: constant.data_type,
            address: constant.address,
            units: constant.units.literal().unwrap_or_default().to_owned(),
            scale: attributes.scale,
            translate: attributes.translate,
            minimum: attributes.minimum,
            maximum: attributes.maximum,
            digits: attributes.digits,
            controller_owned,
            no_msq_save: constant.flags.no_msq_save,
            default_raw: 0.0,
        };
        let default_raw = default_raw(definition, name, &spec, values);
        self.insert(VariableSpec {
            default_raw,
            ..spec
        });
    }

    fn add_pc_variable(
        &mut self,
        definition: &Definition,
        name: &str,
        variable: &oxtuner_ini::PcVariable,
        values: &dyn Fn(&str) -> Option<f64>,
    ) {
        let Some(shape) = shape_of(&variable.shape) else {
            self.issues.push(LayoutIssue::new(
                IssueSeverity::Error,
                LayoutIssueKind::UnresolvedShape,
                Some(name),
                "the PC variable declares a shape that cannot be resolved",
            ));
            return;
        };

        let spec = VariableSpec {
            name: name.to_owned(),
            owner: Owner::Pc,
            shape,
            data_type: variable.data_type,
            // PC variables live in the project file, not on a page.
            address: None,
            units: variable.units.literal().unwrap_or_default().to_owned(),
            scale: Some(1.0),
            translate: Some(0.0),
            minimum: None,
            maximum: None,
            digits: 0,
            controller_owned: false,
            no_msq_save: false,
            default_raw: 0.0,
        };
        let default_raw = default_raw(definition, name, &spec, values);
        self.insert(VariableSpec {
            default_raw,
            ..spec
        });
    }

    fn insert(&mut self, spec: VariableSpec) {
        if self.index(&spec.name).is_some() {
            self.issues.push(LayoutIssue::new(
                IssueSeverity::Error,
                LayoutIssueKind::UnresolvedAttribute,
                Some(&spec.name),
                "a second entry with the same name (case-insensitively) was ignored",
            ));
            return;
        }

        let index = self.variables.len();
        self.lookup.entry(ci_hash(&spec.name)).or_insert(index);
        self.variables.push(spec);
    }

    fn build_pages(&mut self, definition: &Definition) {
        let mut pages: Vec<PageLayout> = definition
            .pages
            .iter()
            .map(|page| PageLayout {
                // The definition layer only produces 1-based page numbers; a
                // zero here would be a definition bug, not a tune concern.
                page: IniPage::new(page.number).unwrap_or(IniPage::FIRST),
                size: page.size,
                start_offset: page.start_offset,
                variables: Vec::new(),
            })
            .collect();
        pages.sort_by_key(|page| page.page);

        for (index, variable) in self.variables.iter().enumerate() {
            let Some(page) = variable.ini_page() else {
                continue;
            };
            if let Some(entry) = pages.iter_mut().find(|candidate| candidate.page == page) {
                entry.variables.push(index);
            } else {
                self.issues.push(LayoutIssue::new(
                    IssueSeverity::Warning,
                    LayoutIssueKind::UnknownPage,
                    Some(&variable.name),
                    format!(
                        "the definition puts this constant on page {page}, which it never \
                         declares; the constant stays readable but has no page size"
                    ),
                ));
                pages.push(PageLayout {
                    page,
                    size: 0,
                    start_offset: 0,
                    variables: vec![index],
                });
            }
        }

        pages.sort_by_key(|page| page.page);
        self.pages = pages;
    }

    /// The firmware signature the layout was built for.
    pub fn signature(&self) -> &str {
        &self.signature
    }

    /// The byte order calibration values use.
    pub fn endianness(&self) -> Endianness {
        self.endianness
    }

    /// Every variable, in definition order (constants first, then PC variables).
    pub fn variables(&self) -> &[VariableSpec] {
        &self.variables
    }

    /// One variable by exact name.
    pub fn variable(&self, name: &str) -> Option<&VariableSpec> {
        self.index(name).map(|index| &self.variables[index])
    }

    /// The index of a variable by name, case-insensitively.
    pub fn index(&self, name: &str) -> Option<usize> {
        let hash = ci_hash(name);
        if let Some(&index) = self.lookup.get(&hash) {
            let variable = &self.variables[index];
            if variable.name.eq_ignore_ascii_case(name) {
                return Some(index);
            }

            // A hash collision stored another name first; the linear scan is
            // the oracle for that case.
            return self
                .variables
                .iter()
                .position(|variable| variable.name.eq_ignore_ascii_case(name));
        }

        None
    }

    /// Calibration pages, ordered by page number.
    pub fn pages(&self) -> &[PageLayout] {
        &self.pages
    }

    /// One page by INI number.
    pub fn page(&self, page: IniPage) -> Option<&PageLayout> {
        self.pages.iter().find(|candidate| candidate.page == page)
    }

    /// The page a variable belongs to.
    pub fn page_of(&self, variable: usize) -> Option<&PageLayout> {
        let page = self.variables.get(variable)?.ini_page()?;
        self.page(page)
    }

    /// Every problem found while building the layout.
    pub fn issues(&self) -> &[LayoutIssue] {
        &self.issues
    }

    /// Total number of stored cells across every variable.
    pub fn cell_count(&self) -> usize {
        self.variables.iter().map(VariableSpec::element_count).sum()
    }

    /// Number of calibration pages.
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
}

/// One constant's resolved presentation attributes.
struct Attributes {
    scale: Option<f64>,
    translate: Option<f64>,
    minimum: Option<f64>,
    maximum: Option<f64>,
    digits: u8,
    unresolved: Option<&'static str>,
}

impl Attributes {
    fn resolve(
        scale: Option<&Number>,
        translate: Option<&Number>,
        minimum: Option<&Number>,
        maximum: Option<&Number>,
        digits: Option<&Number>,
        values: &dyn Fn(&str) -> Option<f64>,
    ) -> Self {
        let mut unresolved = None;
        let mut resolve = |number: Option<&Number>, attribute: &'static str| {
            let number = number?;
            match number.literal().or_else(|| number.evaluate(values)) {
                Some(value) if value.is_finite() => Some(value),
                _ => {
                    unresolved.get_or_insert(attribute);
                    None
                }
            }
        };

        let scale = if scale.is_some() {
            resolve(scale, "scale")
        } else {
            Some(1.0)
        };
        let translate = if translate.is_some() {
            resolve(translate, "translate")
        } else {
            Some(0.0)
        };
        let minimum = resolve(minimum, "min");
        let maximum = resolve(maximum, "max");
        let digits = resolve(digits, "digits").map_or(0, digit_count);

        Self {
            scale,
            translate,
            minimum,
            maximum,
            digits,
            unresolved,
        }
    }
}

/// A resolved digit count in the range the definition allows.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::as_conversions,
    reason = "the value is rounded and clamped to 0..=9 before the cast"
)]
fn digit_count(value: f64) -> u8 {
    value.round().clamp(0.0, 9.0) as u8
}

fn shape_of(shape: &Shape) -> Option<EntryShape> {
    match shape {
        Shape::Scalar => Some(EntryShape::Scalar),
        Shape::Bits { low, high, shift } => Some(EntryShape::Bits {
            low: *low,
            high: *high,
            shift: *shift,
        }),
        Shape::List(length) => Some(EntryShape::Array {
            cols: usize::try_from(length.value()?).ok()?,
            rows: 1,
        }),
        Shape::Table { cols, rows } => Some(EntryShape::Array {
            cols: usize::try_from(cols.value()?).ok()?,
            rows: usize::try_from(rows.value()?).ok()?,
        }),
        Shape::Text { length, .. } => Some(EntryShape::Text {
            len: usize::try_from(length.value()?).ok()?,
        }),
    }
}

/// The definition's default for one variable, in raw units.
fn default_raw(
    definition: &Definition,
    name: &str,
    spec: &VariableSpec,
    values: &dyn Fn(&str) -> Option<f64>,
) -> f64 {
    if !spec.shape.is_numeric() {
        return 0.0;
    }

    let resolve = |number: &Number| number.literal().or_else(|| number.evaluate(values));

    if let Some(raw) = definition
        .extensions
        .raw_defaults
        .get_ci(name)
        .and_then(resolve)
    {
        return raw;
    }

    if let Some(user) = definition
        .extensions
        .defaults
        .get_ci(name)
        .and_then(resolve)
        && let Ok(conversion) = spec.to_raw(user)
    {
        return conversion.raw;
    }

    0.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use oxtuner_ini::{SettingsContext, parse_str};

    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 16
page = 1
reqFuel = scalar, U08, 0, "", 1, 0, 0, 3, 0
REQFUEL = scalar, U08, 1, "", 1, 0, 0, 3, 0
"#;

    #[test]
    fn the_layout_index_is_case_insensitive_and_keeps_the_first_name() {
        let definition = parse_str(DEFINITION, &SettingsContext::new()).expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);

        let index = layout.index("reqFuel").expect("first spelling");
        assert_eq!(layout.index("REQFUEL"), Some(index));
        assert_eq!(layout.index("ReqFuel"), Some(index));
        assert!(layout.index("missing").is_none());

        assert_eq!(
            layout.variables().len(),
            1,
            "the case-insensitive duplicate is ignored"
        );
        assert!(
            layout
                .issues()
                .iter()
                .any(|issue| issue.message.contains("same name")),
            "the ignored duplicate is reported: {:?}",
            layout.issues()
        );
    }
}
