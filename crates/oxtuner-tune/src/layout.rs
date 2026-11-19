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

use std::collections::{HashMap, HashSet};
use std::fmt;

use oxtuner_ini::expr::EvalContext;
use oxtuner_ini::{ConstantClass, DataType, Definition, Endianness, Number, Shape};

use crate::{IniPage, Tune, TuneError};

/// An [`EvalContext`] over a plain name lookup.
struct FnContext<'a> {
    lookup: &'a dyn Fn(&str) -> Option<f64>,
}

impl EvalContext for FnContext<'_> {
    fn value(&mut self, name: &str) -> Option<f64> {
        (self.lookup)(name)
    }
}

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
///
/// Every field is public and the struct is not `#[non_exhaustive]`, so a
/// caller that builds one owns the invariants: `shape` and `data_type` must
/// agree on numeric versus text storage, and `address` must fit the page it
/// names. The layout builder is the implementation that enforces them.
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

/// One numeric constant's byte range on a page.
#[derive(Debug, Clone, Copy)]
struct Span {
    /// Index into the layout's variables.
    index: usize,
    /// First byte, inclusive.
    start: u32,
    /// Number of bytes covered.
    len: u32,
}

impl Span {
    /// First byte past the end.
    fn end(self) -> u32 {
        self.start.saturating_add(self.len)
    }

    /// Whether this span holds every byte of `other`.
    fn covers(self, other: Self) -> bool {
        self.start <= other.start && other.end() <= self.end()
    }

    /// Whether the two spans share at least one byte.
    fn intersects(self, other: Self) -> bool {
        self.start < other.end() && other.start < self.end()
    }
}

/// The byte range every numeric constant on `page` occupies.
///
/// Text is left out on purpose: [`TuneLayout::page_has_text_overlap`] treats a
/// text field as a blob that owns its bytes, not as a numeric constant that can
/// be merged with a neighbor.
fn page_spans(variables: &[VariableSpec], page: &PageLayout) -> Vec<Span> {
    page.variables
        .iter()
        .filter_map(|index| {
            let variable = variables.get(*index)?;
            if matches!(variable.shape, EntryShape::Text { .. }) {
                return None;
            }

            span_of(*index, variable)
        })
        .collect()
}

/// The bytes one variable occupies on its page, text included.
fn span_of(index: usize, variable: &VariableSpec) -> Option<Span> {
    let start = variable.address?;
    let width = variable.data_type.size();
    let len = match variable.shape {
        EntryShape::Scalar | EntryShape::Bits { .. } => width,
        EntryShape::Array { .. } => {
            width.checked_mul(u32::try_from(variable.element_count()).ok()?)?
        }
        EntryShape::Text { len } => u32::try_from(len).ok()?,
    };

    (len > 0).then_some(Span { index, start, len })
}

/// Marks the variables whose bytes another variable on the same page covers.
///
/// The wider span wins, and containment must be strict. Two constants over
/// exactly the same bytes are peers, not an owner and a view, so the one
/// declared later can still write them. A partial overlap is not an alias and
/// is reported by [`mark_ambiguous`] instead.
///
/// This is deliberately not the dialect's load order: a file applies constants
/// in the order it lists them, while here the wider constant owns the bytes
/// whatever the declaration order. The narrower constant is a view either way,
/// and the definitions this build reads always declare the owner first.
fn mark_aliases(mut spans: Vec<Span>, variables: &[VariableSpec], aliases: &mut [Option<usize>]) {
    // Wider first, declaration order on a tie.
    spans.sort_by(|left, right| right.len.cmp(&left.len).then(left.index.cmp(&right.index)));

    for position in 0..spans.len() {
        let span = spans[position];
        let variable = &variables[span.index];
        // A bit field that covers only part of its word is a real control of
        // those bits, not a duplicate: writing it must still merge into the
        // word. Only a field that spans the whole word duplicates it.
        let duplicate = match variable.shape {
            EntryShape::Bits { low, high, .. } => {
                let bits = variable.data_type.size().saturating_mul(8);
                low == 0 && u32::from(high).saturating_add(1) >= bits && bits > 0
            }
            _ => true,
        };
        if !duplicate {
            continue;
        }

        // Strict containment: two variables over the same bytes are peers, and
        // the one declared later must still be able to write them.
        let owner = spans[..position].iter().find(|outer| {
            outer.len > span.len && outer.start <= span.start && span.end() <= outer.end()
        });
        if let Some(owner) = owner {
            aliases[span.index] = Some(owner.index);
        }
    }
}

/// How many overlapping pairs one page lists before the summary takes over.
const MAX_OVERLAP_ISSUES: usize = 8;

/// Marks variables whose bytes overlap a peer with no safe owner.
///
/// A pair is genuine ambiguity when the ranges intersect, neither covers the
/// other, and neither is a view. Bit fields are covered by the containment
/// rule: a field over the same word has the same span, and a field whose word
/// is contained in a wider constant is a merge. A bit word that reaches into a
/// neighbor's bytes is still reported, because the codec would decide those
/// bytes by declaration order.
///
/// A page can hold many such pairs when a definition is badly formed, so the
/// issues stop after [`MAX_OVERLAP_ISSUES`] and one summary line follows.
fn mark_ambiguous(
    variables: &[VariableSpec],
    mut spans: Vec<Span>,
    page: &PageLayout,
    aliases: &[Option<usize>],
    ambiguous: &mut [bool],
    issues: &mut Vec<LayoutIssue>,
) {
    // Sorted by start, the inner loop can stop at the first span that begins
    // past this one's end.
    spans.sort_by_key(|span| span.start);
    let mut listed = 0usize;
    let mut unlisted = 0usize;

    for (position, span) in spans.iter().enumerate() {
        for other in &spans[position + 1..] {
            if other.start >= span.end() {
                break;
            }
            if !span.intersects(*other) {
                continue;
            }
            // A view is never written; its owner's span is what a write
            // touches, and that pair is checked on its own.
            if aliases.get(span.index).is_some_and(Option::is_some)
                || aliases.get(other.index).is_some_and(Option::is_some)
            {
                continue;
            }
            // Containment is the owner/view or the bit-field merge case.
            if span.covers(*other) || other.covers(*span) {
                continue;
            }

            ambiguous[span.index] = true;
            ambiguous[other.index] = true;
            if listed == MAX_OVERLAP_ISSUES {
                unlisted += 1;
                continue;
            }
            listed += 1;

            let first = &variables[span.index];
            let second = &variables[other.index];
            issues.push(LayoutIssue::new(
                IssueSeverity::Warning,
                LayoutIssueKind::AmbiguousOverlap,
                Some(&first.name),
                format!(
                    "shares bytes {}..{} with `{}` on page {} and neither covers the other; \
                     writing page {} is refused until the definition changes",
                    span.start.max(other.start),
                    span.end().min(other.end()),
                    second.name,
                    page.page,
                    page.page,
                ),
            ));
        }
    }

    if unlisted > 0 {
        issues.push(LayoutIssue::new(
            IssueSeverity::Warning,
            LayoutIssueKind::AmbiguousOverlap,
            None,
            format!(
                "page {} has {unlisted} more overlapping pair(s), not listed",
                page.page
            ),
        ));
    }
}

/// What one layout build decided about shared bytes.
struct OverlapCheck {
    aliases: Vec<Option<usize>>,
    ambiguous: Vec<bool>,
    issues: Vec<LayoutIssue>,
}

fn check_overlaps(variables: &[VariableSpec], pages: &[PageLayout]) -> OverlapCheck {
    let mut check = OverlapCheck {
        aliases: vec![None; variables.len()],
        ambiguous: vec![false; variables.len()],
        issues: Vec::new(),
    };

    for page in pages {
        let spans = page_spans(variables, page);
        mark_aliases(spans.clone(), variables, &mut check.aliases);
        mark_ambiguous(
            variables,
            spans,
            page,
            &check.aliases,
            &mut check.ambiguous,
            &mut check.issues,
        );
    }

    check
}

/// Whether a bit field survives the page codec's read-modify-write round trip.
///
/// The word must be an integer that `f64` holds exactly (at most 4 bytes), and
/// the field must end inside that word. Signed words are supported through
/// their two's-complement bit pattern, which is what the real definitions use.
/// Wider and floating-point words are not, because the read-modify-write path
/// would round them.
pub(crate) fn bit_field_can_round_trip(data_type: DataType, low: u8, high: u8) -> bool {
    let integer = matches!(
        data_type,
        DataType::U08
            | DataType::S08
            | DataType::U16
            | DataType::S16
            | DataType::U32
            | DataType::S32
            | DataType::M08
    );
    let width = usize::try_from(data_type.size()).unwrap_or(usize::MAX);

    integer && width <= 4 && low <= high && usize::from(high) < width.saturating_mul(8)
}

impl VariableSpec {
    /// Whether the page codec can read and write this variable's bit field.
    ///
    /// A refused field is skipped by the codec, so the write plan must not
    /// offer it as a change that can be sent.
    pub fn bit_field_round_trips(&self) -> bool {
        match self.shape {
            EntryShape::Bits { low, high, .. } => {
                bit_field_can_round_trip(self.data_type, low, high)
            }
            _ => true,
        }
    }

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
///
/// Every field is public and the struct is not `#[non_exhaustive]`. The
/// `variables` indices are meaningful only against the [`TuneLayout`] that
/// built the page, so a caller that constructs one must keep the pair
/// together; [`Self::cell_count`] ignores an index the layout does not have.
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
    ///
    /// An index the layout does not have contributes nothing: the page may
    /// outlive the layout it was built from, and a stale index is not worth a
    /// panic in a view. A debug build asserts the mismatch so the layout bug
    /// behind it does not hide as a short scrollbar.
    pub fn cell_count(&self, layout: &TuneLayout) -> usize {
        self.variables
            .iter()
            .filter_map(|index| {
                let spec = layout.variables.get(*index);
                debug_assert!(spec.is_some(), "stale variable index {index}");
                spec
            })
            .map(VariableSpec::element_count)
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
    reason = "layout issues are a closed set of definition findings"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutIssueKind {
    /// A constant was declared before any `page =` line.
    NoPage,
    /// An array or text dimension is an expression this build cannot fold.
    UnresolvedShape,
    /// The shape resolved, but it needs more cells than this build accepts.
    ShapeTooLarge,
    /// A presentation attribute (`scale`, `translate`, …) is an expression
    /// that did not evaluate against the loaded values.
    UnresolvedAttribute,
    /// A constant names a page the definition never declared.
    UnknownPage,
    /// Two numeric constants share bytes and neither covers the other, so no
    /// single value owns them.
    AmbiguousOverlap,
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

/// Two variables that hold values the shared bytes cannot both carry.
///
/// Sharing bytes is legal: a bit field merges into a wider word, and several
/// fields can split one word between them. What is not legal is two values
/// that disagree about a bit they both cover, because one page image can hold
/// only one of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueConflict {
    /// The page holding the shared bytes.
    pub page: IniPage,
    /// The first variable, in definition order.
    pub first: String,
    /// The second variable, in definition order.
    pub second: String,
    /// First byte of the range the two share.
    pub start: u32,
    /// First byte past the range the two share.
    pub end: u32,
}

impl fmt::Display for ValueConflict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "`{}` and `{}` hold different values for the bytes they share at {}..{} of page {}",
            self.first, self.second, self.start, self.end, self.page
        )
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
    /// One owner per variable, when another variable's bytes contain all of its
    /// own.
    ///
    /// A derived bit alias is the common shape. Such a variable is a view of
    /// the bytes, so it is not persisted to a tune file and not written to the
    /// ECU on its own; the variable that covers it carries the value.
    aliases: Vec<Option<usize>>,
    /// One flag per variable, set when it shares bytes with a peer in a way no
    /// owner resolves. A page carrying such a variable is never written.
    ambiguous: Vec<bool>,
}

impl TuneLayout {
    /// Builds the layout from a definition.
    ///
    /// `values` supplies **user-unit** values for names referenced by
    /// presentation expressions; pass `&|_| None` when no tune is loaded yet.
    /// Missing references are reported in [`Self::issues`], never guessed.
    pub fn build(definition: &Definition, values: &dyn Fn(&str) -> Option<f64>) -> Self {
        let mut context = FnContext { lookup: values };
        Self::build_with_context(definition, &mut context)
    }

    /// Builds the layout against a full evaluation context.
    ///
    /// Use this when presentation attributes need more than a plain name
    /// lookup: arrays (`{ arrayValue(...) }`) and definition-defined functions
    /// (`{ getChannelScaleByOffset(6) }`) only resolve through an
    /// [`EvalContext`].
    pub fn build_with_context(definition: &Definition, values: &mut dyn EvalContext) -> Self {
        let mut layout = Self {
            signature: definition.signature.clone(),
            endianness: definition.endianness,
            variables: Vec::new(),
            lookup: HashMap::new(),
            pages: Vec::new(),
            issues: Vec::new(),
            aliases: Vec::new(),
            ambiguous: Vec::new(),
        };

        for (name, constant) in definition.constants.iter() {
            layout.add_constant(definition, name, constant, values);
        }

        for (name, variable) in definition.pc_variables.iter() {
            layout.add_pc_variable(definition, name, variable, values);
        }

        layout.build_pages(definition);
        let overlap = check_overlaps(&layout.variables, &layout.pages);
        layout.aliases = overlap.aliases;
        layout.ambiguous = overlap.ambiguous;
        layout.issues.extend(overlap.issues);
        layout
    }

    fn cap_shape(&mut self, name: &str, shape: EntryShape) -> Option<EntryShape> {
        let cells = shape.element_count();
        if cells <= MAX_VARIABLE_CELLS {
            return Some(shape);
        }

        self.issues.push(LayoutIssue::new(
            IssueSeverity::Error,
            LayoutIssueKind::ShapeTooLarge,
            Some(name),
            format!(
                "the shape needs {cells} cells, over the {MAX_VARIABLE_CELLS} limit, so the \
                 tune leaves the variable out"
            ),
        ));
        None
    }

    fn add_constant(
        &mut self,
        definition: &Definition,
        name: &str,
        constant: &oxtuner_ini::Constant,
        values: &mut dyn EvalContext,
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
                 constant is left out",
            ));
            return;
        };
        let Some(shape) = self.cap_shape(name, shape) else {
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
        values: &mut dyn EvalContext,
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
        let Some(shape) = self.cap_shape(name, shape) else {
            return;
        };

        // A PC variable declares the same presentation attributes as a
        // constant, so a scaled one keeps its user values instead of being
        // rounded to whole numbers on save.
        let attributes = Attributes::resolve(
            variable.scale.as_ref(),
            variable.translate.as_ref(),
            variable.minimum.as_ref(),
            variable.maximum.as_ref(),
            variable.digits.as_ref(),
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

        let spec = VariableSpec {
            name: name.to_owned(),
            owner: Owner::Pc,
            shape,
            data_type: variable.data_type,
            // PC variables live in the project file, not on a page.
            address: None,
            units: variable.units.literal().unwrap_or_default().to_owned(),
            scale: attributes.scale,
            translate: attributes.translate,
            minimum: attributes.minimum,
            maximum: attributes.maximum,
            digits: attributes.digits,
            controller_owned: variable.flags.controller_priority
                || variable.class == ConstantClass::OddArray,
            no_msq_save: variable.flags.no_msq_save,
            default_raw: 0.0,
        };
        let default_raw = default_raw(definition, name, &spec, values);
        self.insert(VariableSpec {
            default_raw,
            ..spec
        });
    }

    fn insert(&mut self, spec: VariableSpec) {
        // One hash per variable: the duplicate check and the index entry fold
        // the same name.
        let hash = ci_hash(&spec.name);
        if self.find(hash, &spec.name).is_some() {
            self.issues.push(LayoutIssue::new(
                IssueSeverity::Error,
                LayoutIssueKind::UnresolvedAttribute,
                Some(&spec.name),
                "a second entry with the same name (case-insensitively) was ignored",
            ));
            return;
        }

        let index = self.variables.len();
        self.lookup.entry(hash).or_insert(index);
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
        self.find(ci_hash(name), name)
    }

    /// The index of a variable whose folded name hashes to `hash`.
    fn find(&self, hash: u64, name: &str) -> Option<usize> {
        let index = *self.lookup.get(&hash)?;
        let variable = &self.variables[index];
        if variable.name.eq_ignore_ascii_case(name) {
            return Some(index);
        }

        // A hash collision stored another name first; the linear scan is the
        // oracle for that case.
        self.variables
            .iter()
            .position(|variable| variable.name.eq_ignore_ascii_case(name))
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

    /// Whether the variable at `index` is covered by another variable's bytes.
    pub fn is_alias_at(&self, index: usize) -> bool {
        self.aliases.get(index).is_some_and(Option::is_some)
    }

    /// Whether `name` is covered by another variable's bytes.
    pub fn is_alias(&self, name: &str) -> bool {
        self.index(name)
            .is_some_and(|index| self.is_alias_at(index))
    }

    /// The index of the variable that owns `index`'s bytes, when it is a view.
    pub fn alias_owner_at(&self, index: usize) -> Option<usize> {
        self.aliases.get(index).copied().flatten()
    }

    /// The variable that owns `name`'s bytes, when it is a view.
    pub fn alias_owner(&self, name: &str) -> Option<&str> {
        let index = self.index(name)?;
        let owner = self.alias_owner_at(index)?;

        self.variables.get(owner).map(|spec| spec.name.as_str())
    }

    /// Whether the variable at `index` shares bytes no owner resolves.
    pub fn is_ambiguous_at(&self, index: usize) -> bool {
        self.ambiguous.get(index).copied().unwrap_or(false)
    }

    /// Whether `page` holds constants whose shared bytes have no safe owner.
    ///
    /// Such a page is still read and decoded, and a file's values are adopted
    /// as they stand. The core refuses to write the page: the codec would fill
    /// the shared bytes in declaration order, and only a read-back would show
    /// which value survived.
    pub fn page_has_ambiguous_overlap(&self, page: IniPage) -> bool {
        self.page(page).is_some_and(|page_layout| {
            page_layout
                .variables
                .iter()
                .any(|index| self.is_ambiguous_at(*index))
        })
    }

    /// The names on `page` that share bytes with no owner, and how many exist
    /// before the list is cut to [`MAX_OVERLAP_ISSUES`].
    ///
    /// The list is bounded so a refusal message stays readable on a badly
    /// formed definition; subtract its length from the count to say how many
    /// names the message leaves out.
    pub fn ambiguous_names(&self, page: IniPage) -> (Vec<&str>, usize) {
        let Some(page_layout) = self.page(page) else {
            return (Vec::new(), 0);
        };

        let names: Vec<&str> = page_layout
            .variables
            .iter()
            .filter(|index| self.is_ambiguous_at(**index))
            .filter_map(|index| self.variables.get(*index))
            .map(|spec| spec.name.as_str())
            .collect();
        let total = names.len();

        (names.into_iter().take(MAX_OVERLAP_ISSUES).collect(), total)
    }

    /// The variable pairs on `page` whose values cannot both fit their shared bytes.
    ///
    /// Two constants may share storage and still be legal: a bit field merges
    /// into a word another constant fills, and several fields can split one
    /// word between them. The pairs reported here are the ones where both ask
    /// to own the same bit and ask for different values there. A write would
    /// settle them by declaration order, so the caller refuses instead of
    /// letting the codec pick a winner.
    ///
    /// A view is left out: its owner carries the value and the view follows the
    /// decoded page. A pair that neither covers the other is not reported here
    /// either, because [`Self::page_has_ambiguous_overlap`] already refuses
    /// that page outright.
    #[must_use]
    pub fn value_conflicts(&self, page: IniPage, tune: &Tune) -> Vec<ValueConflict> {
        let Some(page_layout) = self.page(page) else {
            return Vec::new();
        };

        let spans = page_spans(&self.variables, page_layout);
        let mut conflicts = Vec::new();

        for (position, span) in spans.iter().enumerate() {
            if self.is_alias_at(span.index) {
                continue;
            }

            for other in &spans[position + 1..] {
                if self.is_alias_at(other.index) {
                    continue;
                }
                // One range inside the other, equal ranges included. Anything
                // else has no owner at all and is refused page-wide already.
                if !(span.covers(*other) || other.covers(*span)) {
                    continue;
                }

                let (first, second) = (&self.variables[span.index], &self.variables[other.index]);
                let (Some(first_claim), Some(second_claim)) = (
                    crate::wire::claim_of(first, self.endianness, tune),
                    crate::wire::claim_of(second, self.endianness, tune),
                ) else {
                    continue;
                };
                if first_claim.agrees_with(&second_claim) {
                    continue;
                }

                conflicts.push(ValueConflict {
                    page,
                    first: first.name.clone(),
                    second: second.name.clone(),
                    start: span.start.max(other.start),
                    end: span.end().min(other.end()),
                });
            }
        }

        conflicts
    }

    /// Copies each view's value on `page` from the variable that owns its bytes.
    ///
    /// Used for a page the codec cannot rebuild: without a decode the view has
    /// no chance to follow its owner and would keep its definition default. A
    /// view the file wrote while its owner stayed home keeps the file's value;
    /// when the file carries the owner, the owner decides and the view follows.
    ///
    /// # Errors
    ///
    /// Returns [`TuneError::LayoutMismatch`] when `tune` was built from another
    /// layout, and [`TuneError`] when a mapped value cannot be stored.
    pub fn derive_views(
        &self,
        page: IniPage,
        tune: &mut Tune,
        carried: &[String],
    ) -> Result<(), TuneError> {
        if !std::ptr::eq(tune.layout().as_ref(), self) {
            return Err(TuneError::LayoutMismatch);
        }
        let Some(page_layout) = self.page(page) else {
            return Ok(());
        };

        for index in &page_layout.variables {
            let Some(owner) = self.alias_owner_at(*index) else {
                continue;
            };
            let (Some(view), Some(owner_spec)) =
                (self.variables.get(*index), self.variables.get(owner))
            else {
                continue;
            };
            // A view mirrors the owner only when both store the same kind of
            // value; a different width or signedness has no cell-to-cell map.
            if view.data_type != owner_spec.data_type {
                continue;
            }
            let view_carried = carried
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&view.name));
            let owner_carried = carried
                .iter()
                .any(|name| name.eq_ignore_ascii_case(&owner_spec.name));
            if view_carried && !owner_carried {
                continue;
            }

            let (Some(view_start), Some(owner_start)) = (view.address, owner_spec.address) else {
                continue;
            };
            let width = view.data_type.size();
            if width == 0 {
                continue;
            }
            // An unaligned start has no owner element to mirror.
            let Some(base) = view_start.checked_sub(owner_start) else {
                continue;
            };
            if base % width != 0 {
                continue;
            }
            let Some(first) = usize::try_from(base / width).ok() else {
                continue;
            };

            for element in 0..view.element_count() {
                let Some(owner_index) = first.checked_add(element) else {
                    continue;
                };
                if owner_index >= owner_spec.element_count() {
                    continue;
                }
                let Ok(raw) = tune.raw_at(&owner_spec.name, owner_index) else {
                    continue;
                };
                // A bit field reads its word's two's-complement pattern, not the
                // owner's signed value.
                let raw = match view.shape {
                    EntryShape::Bits { low, high, .. } => {
                        let Some(bits) = crate::wire::extract_bits(raw, low, high) else {
                            continue;
                        };

                        f64::from(bits)
                    }
                    _ => raw,
                };

                tune.set_image_raw(&view.name, element, raw)?;
            }
        }

        Ok(())
    }

    /// Whether a text field on `page` shares bytes with another variable.
    ///
    /// The page codec reads a text field as its whole length. A number that
    /// overlaps it would be read as part of the text, so such a page is left as
    /// the file wrote it instead of being rebuilt.
    pub fn page_has_text_overlap(&self, page: IniPage) -> bool {
        let Some(page_layout) = self.page(page) else {
            return false;
        };

        let spans: Vec<(Span, bool)> = page_layout
            .variables
            .iter()
            .filter_map(|index| {
                let spec = self.variables.get(*index)?;
                let span = span_of(*index, spec)?;

                Some((span, matches!(spec.shape, EntryShape::Text { .. })))
            })
            .collect();

        spans.iter().any(|(span, text)| {
            *text
                && spans
                    .iter()
                    .any(|(other, _)| span.index != other.index && span.intersects(*other))
        })
    }

    /// The names `page` patches when a file carried `applied`.
    ///
    /// A view is included only when the file did not carry its owner: the owner
    /// writes the bytes and the view follows the decoded page. Keeping this in
    /// one place means a load and an import cannot disagree.
    pub fn page_patch_names<'a>(&self, applied: &'a [String], page: IniPage) -> Vec<&'a str> {
        let applied_names: HashSet<&str> = applied.iter().map(String::as_str).collect();

        applied
            .iter()
            .map(String::as_str)
            .filter(|name| {
                self.variable(name)
                    .is_some_and(|spec| spec.ini_page() == Some(page))
            })
            .filter(|name| {
                self.alias_owner(name)
                    .is_none_or(|owner| !applied_names.contains(owner))
            })
            .collect()
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
        values: &mut dyn EvalContext,
    ) -> Self {
        let mut unresolved = None;
        let mut resolve = |number: Option<&Number>, attribute: &'static str| {
            let number = number?;
            match number.literal().or_else(|| number.evaluate_with(values)) {
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

/// Largest number of cells a variable may have.
///
/// Real tables are at most a few thousand cells. A hostile definition can
/// declare billions, which would abort on allocation when the tune is built.
/// Text length is not counted here: it is bounded by the page buffer when a
/// value is read or written, and the tune stores only the actual string.
const MAX_VARIABLE_CELLS: usize = 65_536;

fn shape_of(shape: &Shape) -> Option<EntryShape> {
    let entry = match shape {
        Shape::Scalar => EntryShape::Scalar,
        Shape::Bits { low, high, shift } => EntryShape::Bits {
            low: *low,
            high: *high,
            shift: *shift,
        },
        Shape::List(length) => EntryShape::Array {
            cols: usize::try_from(length.value()?).ok()?,
            rows: 1,
        },
        Shape::Table { cols, rows } => EntryShape::Array {
            cols: usize::try_from(cols.value()?).ok()?,
            rows: usize::try_from(rows.value()?).ok()?,
        },
        Shape::Text { length, .. } => EntryShape::Text {
            len: usize::try_from(length.value()?).ok()?,
        },
    };

    Some(entry)
}

/// The definition's default for one variable, in raw units.
fn default_raw(
    definition: &Definition,
    name: &str,
    spec: &VariableSpec,
    values: &mut dyn EvalContext,
) -> f64 {
    if !spec.shape.is_numeric() {
        return 0.0;
    }

    let resolve = |number: &Number, values: &mut dyn EvalContext| {
        number.literal().or_else(|| number.evaluate_with(values))
    };

    if let Some(raw) = definition
        .extensions
        .raw_defaults
        .get_ignore_case(name)
        .and_then(|number| resolve(number, values))
    {
        return spec.clamp_raw(raw);
    }

    if let Some(user) = definition
        .extensions
        .defaults
        .get_ignore_case(name)
        .and_then(|number| resolve(number, values))
        && let Ok(conversion) = spec.to_raw(user)
    {
        return spec.clamp_raw(conversion.raw);
    }

    // No declared default. A raw zero is the right default only when it reads
    // back as user zero inside the declared range: a translate offset moves it,
    // and the fast path would return exactly the value the clamp exists to
    // avoid. Zero is inside every storage type, so that half needs no check.
    let zero_in_range = spec.translate == Some(0.0)
        && 0.0 >= spec.minimum.unwrap_or(f64::NEG_INFINITY)
        && 0.0 <= spec.maximum.unwrap_or(f64::INFINITY);
    if zero_in_range {
        debug_assert_eq!(
            0.0,
            spec.clamp_raw(0.0),
            "the fast path must return what the clamp would"
        );

        return 0.0;
    }

    spec.clamp_raw(0.0)
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;
    use std::sync::Arc;

    use super::*;
    use oxtuner_ini::{SettingsContext, parse_str};

    #[test]
    fn a_number_inside_a_text_field_flags_the_page() {
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 16
page = 1
note = string, ASCII, 8
inside = scalar, U08, 4, \"x\", 1, 0
outside = scalar, U08, 12, \"x\", 1, 0
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);

        assert!(
            layout.page_has_text_overlap(IniPage::FIRST),
            "the scalar at 4 sits inside the eight text bytes"
        );
    }

    #[test]
    fn a_text_field_beside_a_number_is_not_an_overlap() {
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 16
page = 1
note = string, ASCII, 8
after = scalar, U08, 8, \"x\", 1, 0
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);

        assert!(!layout.page_has_text_overlap(IniPage::FIRST));
    }

    #[test]
    fn overlapping_numeric_constants_are_reported_and_refuse_the_page() {
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 16
page = 1
wide = array, U08, 0, [4]
straddle = array, U08, 2, [4]
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);
        let wide = layout.index("wide").expect("wide is declared");
        let straddle = layout.index("straddle").expect("straddle is declared");

        assert!(layout.page_has_ambiguous_overlap(IniPage::FIRST));
        assert!(layout.is_ambiguous_at(wide));
        assert!(layout.is_ambiguous_at(straddle));
        assert!(!layout.is_alias_at(wide), "neither range covers the other");
        assert!(!layout.is_alias_at(straddle));
        assert!(
            layout.issues().iter().any(|issue| {
                issue.kind == LayoutIssueKind::AmbiguousOverlap
                    && issue.name.as_deref() == Some("wide")
            }),
            "the overlap names the variable: {:?}",
            layout.issues()
        );
    }

    #[test]
    fn overlapping_views_do_not_freeze_their_page() {
        // Two views of one owner share bytes with each other but neither is
        // ever written, so the pair cannot make the page unsafe.
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 16
page = 1
owner = array, U08, 0, [10]
left = array, U08, 4, [3]
right = array, U08, 6, [3]
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);

        assert!(layout.is_alias("left"));
        assert!(layout.is_alias("right"));
        assert!(
            !layout.page_has_ambiguous_overlap(IniPage::FIRST),
            "a view is never sent, so its pair is not a write hazard: {:?}",
            layout.issues()
        );
    }

    #[test]
    fn many_overlapping_pairs_report_a_bounded_list() {
        let mut text = String::from(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 32
page = 1
",
        );
        for start in 0..20 {
            text.write_str(&format!("cell_{start} = array, U08, {start}, [4]\n"))
                .expect("the test string is writable");
        }
        let definition = parse_str(&text, &SettingsContext::new()).expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);

        let listed = layout
            .issues()
            .iter()
            .filter(|issue| issue.kind == LayoutIssueKind::AmbiguousOverlap)
            .count();
        assert_eq!(
            listed,
            MAX_OVERLAP_ISSUES + 1,
            "the pairs stop at the cap and one summary line follows"
        );
        assert!(
            layout
                .issues()
                .iter()
                .any(|issue| issue.message.contains("more overlapping")),
            "the summary counts what it did not list"
        );
        assert!(layout.page_has_ambiguous_overlap(IniPage::FIRST));

        let (names, total) = layout.ambiguous_names(IniPage::FIRST);
        assert_eq!(names.len(), MAX_OVERLAP_ISSUES, "the list is bounded");
        assert_eq!(total, 20, "the count still reports every ambiguous cell");
    }

    const VIEWS_DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
endianness = little
nPages = 1
pageSize = 16
page = 1
data = array, U08, 0, [4]
tail = array, U08, 2, [2]
"#;

    #[test]
    fn derive_views_maps_each_element_to_its_owner_cell() {
        let definition = parse_str(VIEWS_DEFINITION, &SettingsContext::new()).expect("parses");
        let layout = Arc::new(TuneLayout::build(&definition, &|_| None));
        let mut tune = Tune::new(Arc::clone(&layout));
        for (index, value) in [1.0, 2.0, 3.0, 4.0].into_iter().enumerate() {
            tune.set_image_raw("data", index, value)
                .expect("the owner takes a value");
        }

        layout
            .derive_views(IniPage::FIRST, &mut tune, &[])
            .expect("views derive");

        assert_eq!(tune.raw_at("tail", 0).expect("the view reads"), 3.0);
        assert_eq!(tune.raw_at("tail", 1).expect("the view reads"), 4.0);
    }

    #[test]
    fn a_carried_view_keeps_the_files_value_only_without_its_owner() {
        let definition = parse_str(VIEWS_DEFINITION, &SettingsContext::new()).expect("parses");
        let layout = Arc::new(TuneLayout::build(&definition, &|_| None));
        let mut tune = Tune::new(Arc::clone(&layout));
        for (index, value) in [1.0, 2.0, 3.0, 4.0].into_iter().enumerate() {
            tune.set_image_raw("data", index, value)
                .expect("the owner takes a value");
        }
        tune.set_image_raw("tail", 0, 9.0)
            .expect("the view takes a value");
        tune.set_image_raw("tail", 1, 9.0)
            .expect("the view takes a value");

        layout
            .derive_views(IniPage::FIRST, &mut tune, &["tail".to_owned()])
            .expect("views derive");
        assert_eq!(
            tune.raw_at("tail", 0).expect("the view reads"),
            9.0,
            "the file wrote the view alone, so its value stands"
        );

        // With the owner carried too, the owner owns the bytes and the view
        // follows, exactly as a writable page does.
        layout
            .derive_views(
                IniPage::FIRST,
                &mut tune,
                &["data".to_owned(), "tail".to_owned()],
            )
            .expect("views derive");
        assert_eq!(tune.raw_at("tail", 0).expect("the view reads"), 3.0);
        assert_eq!(tune.raw_at("tail", 1).expect("the view reads"), 4.0);
    }

    #[test]
    fn derive_views_refuses_a_tune_from_another_layout() {
        let first = parse_str(VIEWS_DEFINITION, &SettingsContext::new()).expect("parses");
        let second = parse_str(VIEWS_DEFINITION, &SettingsContext::new()).expect("parses");
        let layout = TuneLayout::build(&first, &|_| None);
        let other = Arc::new(TuneLayout::build(&second, &|_| None));
        let mut tune = Tune::new(other);

        assert!(matches!(
            layout.derive_views(IniPage::FIRST, &mut tune, &[]),
            Err(TuneError::LayoutMismatch)
        ));
    }

    #[test]
    fn a_bit_word_straddling_a_neighbor_is_ambiguous() {
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 16
page = 1
value = array, U08, 2, [4]
flag = bits, U16, 1, [0:1]
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);

        assert!(
            layout.page_has_ambiguous_overlap(IniPage::FIRST),
            "the bit word covers bytes 1..3 and the array 2..6: {:?}",
            layout.issues()
        );
    }

    #[test]
    fn bit_fields_sharing_a_word_are_not_ambiguous() {
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 16
page = 1
status = scalar, U08, 4, \"bits\", 1, 0
fan = bits, U08, 4, [0:1]
launch = bits, U08, 4, [2:3]
wide = scalar, U16, 6, \"x\", 1, 0
flag = bits, U16, 6, [3:3]
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);

        assert!(
            !layout.page_has_ambiguous_overlap(IniPage::FIRST),
            "the codec merges these fields: {:?}",
            layout.issues()
        );
        assert!(!layout.is_alias("fan"), "a partial bit field is a control");
    }

    #[test]
    fn bit_fields_sharing_a_bit_are_a_merge_not_a_conflict() {
        // A real definition may leave an unused field overlapping a live one on
        // the same byte. The codec merges both in declaration order, so the
        // pair has a defined outcome.
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 16
page = 1
unused = bits, U08, 0, [0:2]
selector = bits, U08, 0, [2:3]
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);

        assert!(
            !layout.page_has_ambiguous_overlap(IniPage::FIRST),
            "shared bits merge in declaration order: {:?}",
            layout.issues()
        );
    }

    const PEERS: &str = "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 16
page = 1
first = scalar, U08, 0, \"x\", 1, 0
second = scalar, U08, 0, \"x\", 1, 0
";

    fn peers() -> (Arc<TuneLayout>, Tune) {
        let definition = parse_str(PEERS, &SettingsContext::new()).expect("parses");
        let layout = Arc::new(TuneLayout::build(&definition, &|_| None));
        let tune = Tune::new(Arc::clone(&layout));

        (layout, tune)
    }

    #[test]
    fn peers_over_the_same_byte_that_disagree_are_reported() {
        let (layout, mut tune) = peers();
        tune.set_image_raw("first", 0, 100.0)
            .expect("the first takes a value");
        tune.set_image_raw("second", 0, 200.0)
            .expect("the second takes a value");

        assert_eq!(
            layout.value_conflicts(IniPage::FIRST, &tune),
            vec![ValueConflict {
                page: IniPage::FIRST,
                first: "first".to_owned(),
                second: "second".to_owned(),
                start: 0,
                end: 1,
            }],
            "one byte cannot hold both values"
        );
    }

    #[test]
    fn peers_over_the_same_byte_that_agree_are_left_alone() {
        let (layout, mut tune) = peers();
        tune.set_image_raw("first", 0, 100.0)
            .expect("the first takes a value");
        tune.set_image_raw("second", 0, 100.0)
            .expect("the second takes a value");

        assert!(
            layout.value_conflicts(IniPage::FIRST, &tune).is_empty(),
            "the two scalars say the same thing about their byte"
        );

        // What the wire stores is what counts, so two raws that round to the
        // same byte are one value, not a disagreement.
        tune.set_image_raw("second", 0, 100.4)
            .expect("the second takes a value");
        assert!(
            layout.value_conflicts(IniPage::FIRST, &tune).is_empty(),
            "100.4 stores as the same byte as 100.0"
        );
    }

    const PEER_ARRAYS: &str = "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 16
page = 1
first = array, U08, 0, [4], \"x\", 1, 0, 0, 255, 0
second = array, U08, 0, [4], \"x\", 1, 0, 0, 255, 0
";

    #[test]
    fn peers_over_the_same_bytes_compare_every_cell() {
        let definition = parse_str(PEER_ARRAYS, &SettingsContext::new()).expect("parses");
        let layout = Arc::new(TuneLayout::build(&definition, &|_| None));
        let mut tune = Tune::new(Arc::clone(&layout));
        for (index, value) in [1.0, 2.0, 3.0, 4.0].into_iter().enumerate() {
            tune.set_image_raw("first", index, value)
                .expect("the first takes a value");
            tune.set_image_raw("second", index, value)
                .expect("the second takes a value");
        }

        assert!(
            layout.value_conflicts(IniPage::FIRST, &tune).is_empty(),
            "two identical arrays agree cell for cell"
        );

        // One cell out of four is enough: the bytes cannot carry both arrays.
        tune.set_image_raw("second", 2, 9.0)
            .expect("the second takes a value");
        assert_eq!(
            layout.value_conflicts(IniPage::FIRST, &tune),
            vec![ValueConflict {
                page: IniPage::FIRST,
                first: "first".to_owned(),
                second: "second".to_owned(),
                start: 0,
                end: 4,
            }]
        );
    }

    const SPLIT_WORD: &str = "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 16
page = 1
low = bits, U08, 0, [0:3]
high = bits, U08, 0, [4:7]
";

    #[test]
    fn bit_fields_that_split_a_word_never_conflict() {
        let definition = parse_str(SPLIT_WORD, &SettingsContext::new()).expect("parses");
        let layout = Arc::new(TuneLayout::build(&definition, &|_| None));
        let mut tune = Tune::new(Arc::clone(&layout));
        tune.set_image_raw("low", 0, 5.0).expect("takes a value");
        tune.set_image_raw("high", 0, 9.0).expect("takes a value");

        assert!(
            layout.value_conflicts(IniPage::FIRST, &tune).is_empty(),
            "no bit belongs to both fields"
        );
    }

    #[test]
    fn bit_fields_that_share_a_bit_must_agree_about_it() {
        // `[0:2]` and `[2:3]` both own bit 2, which is legal as long as both
        // say the same thing about it.
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 16
page = 1
unused = bits, U08, 0, [0:2]
selector = bits, U08, 0, [2:3]
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = Arc::new(TuneLayout::build(&definition, &|_| None));
        let mut tune = Tune::new(Arc::clone(&layout));

        tune.set_image_raw("unused", 0, 5.0).expect("takes a value");
        tune.set_image_raw("selector", 0, 1.0)
            .expect("takes a value");
        assert!(
            layout.value_conflicts(IniPage::FIRST, &tune).is_empty(),
            "both read bit 2 as set"
        );

        // `selector = 2` places its one at bit 3 and leaves bit 2 clear, which
        // is not what `unused = 5` says about that bit.
        tune.set_image_raw("selector", 0, 2.0)
            .expect("takes a value");
        assert_eq!(
            layout.value_conflicts(IniPage::FIRST, &tune),
            vec![ValueConflict {
                page: IniPage::FIRST,
                first: "unused".to_owned(),
                second: "selector".to_owned(),
                start: 0,
                end: 1,
            }],
            "bit 2 cannot be both set and clear"
        );
    }

    #[test]
    fn a_bit_field_and_the_word_it_sits_in_must_agree() {
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 16
page = 1
status = scalar, U08, 4, \"bits\", 1, 0
fan = bits, U08, 4, [0:1]
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);
        let mut tune = Tune::new(Arc::new(TuneLayout::build(&definition, &|_| None)));

        // 0b1010_0101 holds 1 in bits 0..1, so the field agrees with it.
        tune.set_image_raw("status", 0, 165.0)
            .expect("takes a value");
        tune.set_image_raw("fan", 0, 1.0).expect("takes a value");
        assert!(
            layout.value_conflicts(IniPage::FIRST, &tune).is_empty(),
            "the field reads the bits the word already carries"
        );

        tune.set_image_raw("fan", 0, 2.0).expect("takes a value");
        assert_eq!(
            layout.value_conflicts(IniPage::FIRST, &tune),
            vec![ValueConflict {
                page: IniPage::FIRST,
                first: "status".to_owned(),
                second: "fan".to_owned(),
                start: 4,
                end: 5,
            }],
            "the word says 1 where the field says 2"
        );
    }

    #[test]
    fn a_view_follows_its_owner_and_never_conflicts_with_it() {
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 16
page = 1
data = array, U08, 0, [4], \"\", 1, 0, 0, 255, 0
data0 = bits, U08, 0, [0:7]
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);
        let mut tune = Tune::new(Arc::new(TuneLayout::build(&definition, &|_| None)));
        tune.set_image_raw("data", 0, 12.0)
            .expect("the owner takes a value");
        tune.set_image_raw("data0", 0, 99.0)
            .expect("the view takes a value");

        assert!(
            layout.value_conflicts(IniPage::FIRST, &tune).is_empty(),
            "the view is never written, so the owner decides those bytes"
        );
    }

    #[test]
    fn a_full_width_bit_alias_is_a_view_and_a_partial_one_is_not() {
        let definition = oxtuner_ini::Definition::parse(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 8
page = 1
data = array, U08, 0, [4], \"\", 1, 0, 0, 255, 0
data0 = bits, U08, 0, [0:7]
data1 = bits, U08, 1, [0:7]
fan = bits, U08, 4, [1:1]
status = scalar, U08, 4, \"bits\", 1, 0
",
            &oxtuner_ini::SettingsContext::new(),
        )
        .expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);

        assert!(layout.is_alias("data0"), "a whole byte inside the array");
        assert!(layout.is_alias("data1"));
        assert!(!layout.is_alias("data"), "the array owns its bytes");
        assert!(
            !layout.is_alias("fan"),
            "a partial bit field is a control, not a view"
        );
        assert!(!layout.is_alias("status"), "a peer of the bit field");
    }

    #[test]
    fn an_oversized_shape_drops_the_variable_with_an_issue() {
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
nPages = 1
pageSize = 1024
page = 1
big = array, U08, 0, [4000000000x4000000000], \"x\", 1, 0
",
            &SettingsContext::new(),
        )
        .expect("parses");

        let layout = TuneLayout::build(&definition, &|_| None);

        assert!(layout.index("big").is_none(), "the variable is dropped");
        assert!(
            layout.issues().iter().any(|issue| {
                issue.kind == LayoutIssueKind::ShapeTooLarge && issue.name.as_deref() == Some("big")
            }),
            "{:?}",
            layout.issues()
        );
    }

    #[test]
    fn a_shape_at_the_cell_cap_is_kept_and_one_past_it_is_dropped() {
        let definition = |shape: &str| {
            parse_str(
                &format!(
                    "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
nPages = 1
pageSize = 1024
page = 1
table = array, U08, 0, {shape}, \"x\", 1, 0
"
                ),
                &SettingsContext::new(),
            )
            .expect("parses")
        };

        let at_cap = TuneLayout::build(&definition("[256x256]"), &|_| None);
        assert!(at_cap.index("table").is_some());
        assert!(
            !at_cap
                .issues()
                .iter()
                .any(|issue| issue.kind == LayoutIssueKind::ShapeTooLarge)
        );

        let over = TuneLayout::build(&definition("[256x257]"), &|_| None);
        assert!(over.index("table").is_none());
        assert!(
            over.issues()
                .iter()
                .any(|issue| issue.kind == LayoutIssueKind::ShapeTooLarge)
        );
    }

    #[test]
    fn a_pc_variable_keeps_its_declared_attributes() {
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[PcVariables]
bins = array, S16, [3], \"AFR\", 0.1, 0.0, -4.0, 4.0, 1
view = oddArray, U08, [4], \"\", 1, 0, 0, 255, 0, noMsqSave
",
            &SettingsContext::new(),
        )
        .expect("parses");

        let layout = TuneLayout::build(&definition, &|_| None);

        let bins = layout
            .variables()
            .iter()
            .find(|spec| spec.name == "bins")
            .expect("the array is in the layout");
        assert_eq!(bins.scale, Some(0.1));
        assert_eq!(bins.translate, Some(0.0));
        assert_eq!(bins.minimum, Some(-4.0));
        assert_eq!(bins.maximum, Some(4.0));
        assert_eq!(bins.digits, 1);

        let view = layout
            .variables()
            .iter()
            .find(|spec| spec.name == "view")
            .expect("the odd array is in the layout");
        assert!(view.controller_owned, "an oddArray view is read-only");
        assert!(view.no_msq_save, "the entry declares noMsqSave");
    }

    #[test]
    fn an_oversized_pc_variable_shape_is_dropped() {
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[PcVariables]
big = array, U08, [4000000000x4000000000], \"x\", 1, 0
",
            &SettingsContext::new(),
        )
        .expect("parses");

        let layout = TuneLayout::build(&definition, &|_| None);

        assert!(layout.index("big").is_none());
        assert!(
            layout.issues().iter().any(|issue| {
                issue.kind == LayoutIssueKind::ShapeTooLarge && issue.name.as_deref() == Some("big")
            }),
            "{:?}",
            layout.issues()
        );
    }

    #[test]
    fn a_bit_field_must_fit_its_integer_word() {
        assert!(bit_field_can_round_trip(DataType::U08, 0, 7));
        assert!(bit_field_can_round_trip(DataType::U16, 0, 15));
        assert!(bit_field_can_round_trip(DataType::U32, 0, 31));
        assert!(bit_field_can_round_trip(DataType::M08, 0, 7));
        assert!(bit_field_can_round_trip(DataType::S08, 0, 7));
        assert!(bit_field_can_round_trip(DataType::S16, 0, 15));
        assert!(bit_field_can_round_trip(DataType::S32, 0, 31));

        assert!(!bit_field_can_round_trip(DataType::U16, 8, 23));
        assert!(!bit_field_can_round_trip(DataType::U64, 0, 7));
        assert!(!bit_field_can_round_trip(DataType::S64, 0, 7));
        assert!(!bit_field_can_round_trip(DataType::F32, 0, 7));
        assert!(!bit_field_can_round_trip(DataType::U16, 100, 5));
    }

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
    fn a_default_outside_the_written_step_is_pulled_inside() {
        const BOUNDED: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 16
page = 1
p = scalar, F32, 0, "", 0.145038, 0, 7.25, 101526, 0

[ConstantsExtensions]
defaultValue = p, 7.25
"#;

        let definition = parse_str(BOUNDED, &SettingsContext::new()).expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);
        let spec = layout.variable("p").expect("p");

        // The writer carries at least one decimal, so a raw default that reads
        // back as 7.25 exactly would still be written as 7.2, below the bound.
        let written = spec.format_file(spec.default_raw).expect("written");
        assert_eq!(
            written, "7.3",
            "an omitted default is written at the next representable value"
        );
    }

    #[test]
    fn an_undefaulted_value_with_an_offset_reads_like_the_clamp() {
        const OFFSET: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 1
pageSize = 16
page = 1
off = scalar, U08, 0, "", 1, -50, 0, 100, 0
"#;

        let definition = parse_str(OFFSET, &SettingsContext::new()).expect("parses");
        let layout = TuneLayout::build(&definition, &|_| None);
        let spec = layout.variable("off").expect("off");

        assert_eq!(
            spec.default_raw,
            spec.clamp_raw(0.0),
            "the fast path must return what the clamp would"
        );
        assert_eq!(
            spec.default_raw, 50.0,
            "raw 0 would read back as user -50, below the minimum"
        );
    }

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
