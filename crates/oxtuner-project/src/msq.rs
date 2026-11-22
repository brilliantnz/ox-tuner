//! The `.msq` document model over the raw-preserving XML tree.
//!
//! A `.msq` is thin XML: a `<versionInfo>` header, one `<page>` that holds the
//! PC variables (unnumbered, or written `number="-1"`), one `<page number="N">`
//! per calibration page (0-based), and a `<settings>` snapshot. The PC page's
//! position varies. The reference project writes it last with `number="-1"`,
//! while other writers put an unnumbered one first. Values are user units keyed
//! by name; there are no offsets in the file.
//!
//! Two rules make the file layer trustworthy:
//!
//! - **Pages are addressed by their `number` attribute, never by document
//!   order.** The PC page's variable position breaks any "the Nth `<page>` is
//!   page N" assumption (P10 finding 3).
//! - **The document is kept, not rebuilt.** This type exposes values and the
//!   one text mutation the save path needs; everything else — unknown
//!   attributes, comments, the `<bibliography>`, elements from a newer
//!   `TunerStudio` — serializes back byte-for-byte.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use oxtuner_ini::expr::EvalContext;
use oxtuner_ini::{ChannelKind, Definition, SettingsContext, Shape};
use oxtuner_tune::{
    EntryShape, IniPage, MsqPage, Tune, TuneLayout, ValueConflict, Values, VariableSpec,
};

use crate::flags;
use crate::load;
use crate::xml::{Document, Element, Node, XmlError};
use oxtuner_encoding::Latin1Error as EncodeError;

/// A `.msq` page address: the unnumbered PC page, or a numbered calibration page.
#[expect(
    clippy::exhaustive_enums,
    reason = "a file page is either the PC page or a numbered calibration page"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntryPage {
    /// The PC-variable page: unnumbered, or written `number="-1"`.
    Pc,
    /// A numbered calibration page, 0-based as written in the file.
    Numbered(MsqPage),
}

impl fmt::Display for EntryPage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pc => formatter.write_str("the PC page"),
            Self::Numbered(page) => write!(formatter, "page {page}"),
        }
    }
}

/// The address a `<page>` element declares.
///
/// Some writers leave the page unnumbered; others spell it `number="-1"`,
/// which is how the reference project records it. A negative number is
/// therefore the PC page rather than a calibration page. A number that is not
/// an integer at all is left to the consistency check, which blocks it by name.
fn page_address(element: &Element) -> Option<EntryPage> {
    match element.attr("number") {
        Some(number) => match number.trim().parse::<i64>() {
            Ok(number) if number < 0 => Some(EntryPage::Pc),
            Ok(number) => u16::try_from(number)
                .ok()
                .map(|number| EntryPage::Numbered(MsqPage::new(number))),
            Err(_) => None,
        },
        None => Some(EntryPage::Pc),
    }
}

/// Which kind of entry a value came from.
#[expect(
    clippy::exhaustive_enums,
    reason = "an entry is either a constant or a PC variable"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// A calibration constant.
    Constant,
    /// A PC-side variable.
    PcVariable,
}

impl EntryKind {
    /// The element name in the file.
    pub const fn element(self) -> &'static str {
        match self {
            Self::Constant => "constant",
            Self::PcVariable => "pcVariable",
        }
    }
}

/// One value entry, with its text decoded but not interpreted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Where the entry lives.
    pub page: EntryPage,
    /// What kind of entry it is.
    pub kind: EntryKind,
    /// Name as written (empty when the file omitted it).
    pub name: String,
    /// Value text with surrounding whitespace removed and entities decoded.
    pub value: String,
    /// `cols` attribute, when present.
    pub cols: Option<usize>,
    /// `rows` attribute, when present.
    pub rows: Option<usize>,
    /// `digits` attribute, when present.
    pub digits: Option<u8>,
}

impl Entry {
    /// Whether the file omitted the name.
    pub fn is_nameless(&self) -> bool {
        self.name.is_empty()
    }

    /// The value split into whitespace-separated tokens (arrays and scalars).
    pub fn tokens(&self) -> Vec<&str> {
        self.value.split_whitespace().collect()
    }
}

/// The `<versionInfo>` header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionInfo {
    /// `<versionInfo fileFormat="…">`.
    pub file_format: String,
    /// Firmware description as written.
    pub firmware_info: String,
    /// `nPages`, when the attribute is a valid number.
    pub n_pages: Option<u32>,
    /// Firmware signature; empty when the attribute is missing.
    pub signature: String,
}

/// One numbered `<page>` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageHeader {
    /// Page number from the `number` attribute.
    pub page: MsqPage,
    /// Page size from the `size` attribute, in bytes.
    pub size: Option<u32>,
}

/// A parsed `.msq` file.
#[derive(Debug, Clone)]
pub struct Msq {
    document: Document,
}

/// Why a `.msq` could not be parsed or written.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MsqError {
    /// The XML layer rejected the bytes.
    Xml(XmlError),
    /// The text cannot be represented in ISO-8859-1.
    Encoding(EncodeError),
    /// The document parsed, but its root is not `<msq>`.
    RootIsNotMsq {
        /// The root element name that was found.
        found: String,
    },
    /// An edit named an entry the document does not contain.
    EntryNotFound {
        /// The page that was searched.
        page: EntryPage,
        /// The entry name.
        name: String,
    },
    /// A scaffold found a page with no INI counterpart.
    UnknownPage {
        /// The page number with no definition page.
        page: u16,
    },
    /// The definition's defaults could not be settled onto one value per set of
    /// shared bytes.
    ///
    /// The cause is kept as text because this error compares for equality and
    /// [`TuneError`](oxtuner_tune::TuneError) holds floats, so it cannot be
    /// carried as a source here.
    UnsettledDefaults {
        /// Why the settle failed.
        reason: String,
    },
    /// The ECU page image could not be copied onto the scaffold.
    SeedRejected {
        /// Why the copy failed.
        reason: String,
    },
    /// What the scaffold wrote holds two values for bytes they share.
    ///
    /// A clamp or the user-unit rounding can move one peer and not the other
    /// after the settle, so what lands on disk is judged rather than assumed.
    ConflictingDefaults {
        /// The pair and the bytes they share.
        conflict: ValueConflict,
    },
}

impl fmt::Display for MsqError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Xml(error) => write!(formatter, "invalid .msq XML: {error}"),
            Self::Encoding(error) => write!(formatter, "cannot write .msq: {error}"),
            Self::RootIsNotMsq { found } => {
                write!(formatter, "the root element is <{found}>, not <msq>")
            }
            Self::EntryNotFound { page, name } => {
                write!(formatter, "`{name}` does not exist on {page}")
            }
            Self::UnknownPage { page } => {
                write!(
                    formatter,
                    "the definition declares no page for the values found on page {page}"
                )
            }
            Self::UnsettledDefaults { reason } => {
                write!(formatter, "the defaults cannot be settled: {reason}")
            }
            Self::SeedRejected { reason } => {
                write!(
                    formatter,
                    "the ECU page image cannot seed the tune: {reason}"
                )
            }
            Self::ConflictingDefaults { conflict } => {
                write!(
                    formatter,
                    "{conflict}, so a new tune would open holding two values for one byte"
                )
            }
        }
    }
}

impl std::error::Error for MsqError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Xml(error) => Some(error),
            Self::Encoding(error) => Some(error),
            Self::RootIsNotMsq { .. }
            | Self::EntryNotFound { .. }
            | Self::UnknownPage { .. }
            | Self::ConflictingDefaults { .. }
            | Self::UnsettledDefaults { .. }
            | Self::SeedRejected { .. } => None,
        }
    }
}

impl From<XmlError> for MsqError {
    fn from(error: XmlError) -> Self {
        Self::Xml(error)
    }
}

impl From<EncodeError> for MsqError {
    fn from(error: EncodeError) -> Self {
        Self::Encoding(error)
    }
}

/// The file format a scaffolded tune declares, matching the observed writer.
const SCAFFOLD_FILE_FORMAT: &str = "5.0";
/// The namespace observed on tunes the reference writer produces.
const SCAFFOLD_NAMESPACE: &str = "http://www.msefi.com/:msq";

/// How deep a default may chain through other defaults and computed channels.
const MAX_DEFAULT_DEPTH: usize = 16;

/// The definition's declared defaults, as values a scaffold can evaluate.
///
/// A scaffold has no tune values yet, so the only thing a presentation
/// attribute can name is another default from `[ConstantsExtensions]` or a
/// computed channel the definition derives from those defaults.
/// `rawValue` wins over `defaultValue`, matching how the layout picks a
/// variable's own default. A chain that runs away is cut off rather than
/// recursed into.
struct DefaultValues<'a> {
    definition: &'a Definition,
    depth: usize,
}

impl DefaultValues<'_> {
    fn enter(&mut self) -> bool {
        if self.depth >= MAX_DEFAULT_DEPTH {
            return false;
        }
        self.depth += 1;

        true
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }
}

impl EvalContext for DefaultValues<'_> {
    fn value(&mut self, name: &str) -> Option<f64> {
        let definition = self.definition;

        if let Some(number) = definition
            .extensions
            .raw_defaults
            .get_ignore_case(name)
            .or_else(|| definition.extensions.defaults.get_ignore_case(name))
        {
            if let Some(literal) = number.literal() {
                return Some(literal);
            }
            if !self.enter() {
                return None;
            }
            let value = number.evaluate_with(self);
            self.leave();

            return value;
        }

        let Some(channel) = definition.channel(name) else {
            // A declared variable with no `defaultValue` starts at zero, the
            // same fallback the layout uses for its own default. An unknown
            // name stays unresolved.
            let declared = definition.constant(name).is_some()
                || definition.pc_variables.get_ignore_case(name).is_some();

            return declared.then_some(0.0);
        };
        let ChannelKind::Computed(expression) = &channel.kind else {
            return None;
        };
        if !self.enter() {
            return None;
        }
        let value = expression.evaluate_with(self);
        self.leave();

        value
    }
}

/// Writes the definition's per-cell array defaults into a tune.
///
/// The layout resolves a scalar default on its own, but the dialect writes an
/// array default as one value per cell, which `Number` cannot hold. Creation
/// and the open path both call this, so a definition's declared array default
/// has one answer instead of starting from zeros.
pub(crate) fn apply_definition_defaults(tune: &mut Tune, definition: &Definition) {
    let extensions = &definition.extensions;

    for (name, values) in extensions.default_arrays.iter() {
        let Some(spec) = tune.layout().variable(name).cloned() else {
            continue;
        };
        apply_cells(
            tune,
            &spec,
            values
                .iter()
                .map(|user| spec.to_raw(*user).ok().map(|conversion| conversion.raw)),
        );
    }

    // `rawValue` wins over `defaultValue`, the same precedence the layout uses
    // for a scalar default.
    for (name, values) in extensions.raw_default_arrays.iter() {
        let Some(spec) = tune.layout().variable(name).cloned() else {
            continue;
        };
        apply_cells(tune, &spec, values.iter().map(|raw| Some(*raw)));
    }
}

/// Writes one raw value per index, inside the declared range.
///
/// Indices only grow, so the first cell the tune does not have ends the list:
/// a definition default with more values than the array is a definition
/// problem, and the rest cannot be placed anyway.
fn apply_cells(tune: &mut Tune, spec: &VariableSpec, values: impl Iterator<Item = Option<f64>>) {
    for (index, value) in values.enumerate() {
        let Some(value) = value else { continue };

        let raw = spec.clamp_raw(value);
        if let Err(error) = tune.set_image_raw(&spec.name, index, raw) {
            tracing::debug!(
                name = %spec.name,
                index,
                %error,
                "an array default named no cell"
            );
            return;
        }
    }
}

/// Copies the page-backed values of an ECU image onto a scaffold.
///
/// Only `[Constants]` values travel over the page protocol; the PC variables
/// have no page and keep the defaults the layout resolved. Aliases are copied
/// too: they are redundant views of the same bytes, and the scaffold decides
/// later whether to write them.
fn apply_page_seed(tune: &mut Tune, seed: &Tune) -> Result<(), MsqError> {
    let variables: Vec<(String, Values)> = seed
        .layout()
        .variables()
        .iter()
        .filter(|spec| spec.ini_page().is_some())
        .map(|spec| {
            seed.value(&spec.name)
                .map(|values| (spec.name.clone(), values.clone()))
                .ok_or_else(|| MsqError::SeedRejected {
                    reason: format!("the ECU image has no value for `{}`", spec.name),
                })
        })
        .collect::<Result<_, _>>()?;

    for (name, values) in variables {
        match values {
            Values::Numbers(numbers) => {
                for (index, raw) in numbers.iter().enumerate() {
                    tune.set_image_raw(&name, index, *raw).map_err(|error| {
                        MsqError::SeedRejected {
                            reason: error.to_string(),
                        }
                    })?;
                }
            }
            Values::Text(text) => {
                tune.set_image_text(&name, &text)
                    .map_err(|error| MsqError::SeedRejected {
                        reason: error.to_string(),
                    })?;
            }
        }
    }

    Ok(())
}

/// The values a scaffold writes for one variable.
///
/// A default outside the declared range or the storage type would come back
/// different from the tune's first open, so it is written inside both. The
/// value on disk then equals the value the loader reads, which is what makes a
/// created project exactly an opened one.
fn clamp_values(spec: &VariableSpec, values: &Values) -> Values {
    match values {
        Values::Numbers(numbers) => {
            Values::Numbers(numbers.iter().map(|raw| spec.clamp_raw(*raw)).collect())
        }
        Values::Text(text) => Values::Text(text.clone()),
    }
}

/// Whether the definition declares `name` as a one-dimensional list.
///
/// The flattened shape cannot tell a `[N]` list from a `[Nx1]` table: both are
/// one column of N rows in the tune. Only a list is written as `cols="1"`.
fn is_list(definition: &Definition, name: &str) -> bool {
    let shape = definition
        .constant(name)
        .map(|constant| &constant.shape)
        .or_else(|| {
            definition
                .pc_variables
                .get_ignore_case(name)
                .map(|variable| &variable.shape)
        });

    matches!(shape, Some(Shape::List(_)))
}

/// Builds a tune whose layout resolves presentation attributes from the
/// definition's own defaults.
///
/// A scaffold has no values yet, so an attribute written as an expression
/// (`{fuelLoadMax}`, `{0.1/stoich}`) resolves against the definition. A project
/// created from a controller decodes its page image into the same layout, so
/// the copy and the scaffold agree on every variable name, element count and
/// address.
pub(crate) fn image_for(definition: &Definition) -> Tune {
    let mut defaults = DefaultValues {
        definition,
        depth: 0,
    };
    let layout = TuneLayout::build_with_context(definition, &mut defaults);
    Tune::new(Arc::new(layout))
}

impl Msq {
    /// Builds a new tune document with every value at its definition default,
    /// or at the page values of `seed` when one is supplied.
    ///
    /// The document mirrors what the reference writer produces: an unnumbered
    /// PC page first, one numbered page per definition page carrying the
    /// definition's page sizes, and a `<settings>` snapshot of the active
    /// flags. Variables the definition marks `noMsqSave` are left out, the
    /// same rule the save path applies.
    ///
    /// Presentation attributes are evaluated against the definition's declared
    /// defaults, so an expression such as `{fuelLoadMax}` or `{0.1/stoich}`
    /// resolves before the value is written. A value that still cannot be
    /// written is left out and the open that follows reports it as missing.
    ///
    /// `seed` is the ECU page image a project created from a controller was
    /// read from. Its page-backed values replace the defaults; PC variables
    /// keep their defaults because they do not travel over the page protocol.
    ///
    /// This is crate-private: project creation is the only caller.
    ///
    /// # Errors
    ///
    /// Returns [`MsqError::UnknownPage`] when a scaffolded value belongs to a
    /// page the definition does not declare, [`MsqError::Encoding`] when a
    /// written value cannot be represented in ISO-8859-1,
    /// [`MsqError::SeedRejected`] when the ECU image cannot be copied onto the
    /// scaffold, [`MsqError::UnsettledDefaults`] when the defaults cannot be
    /// settled onto one value per set of shared bytes, and
    /// [`MsqError::ConflictingDefaults`] when what the scaffold writes holds
    /// two values for bytes they share.
    pub(crate) fn scaffold(
        definition: &Definition,
        settings: &SettingsContext,
        seed: Option<&Tune>,
    ) -> Result<Self, MsqError> {
        let mut tune = image_for(definition);
        apply_definition_defaults(&mut tune, definition);
        if let Some(seed) = seed {
            apply_page_seed(&mut tune, seed)?;
        }
        // A definition can give two constants the same bytes and disagree with
        // itself about what those bytes should hold. A new project must not be
        // born with a pair no write would carry, so the defaults are settled
        // through the page image before any of them is written out.
        tune.settle_shared_bytes()
            .map_err(|error| MsqError::UnsettledDefaults {
                reason: error.to_string(),
            })?;
        let layout = Arc::clone(tune.layout());

        let mut pc_entries: Vec<Element> = Vec::new();
        let mut pages: BTreeMap<u16, Vec<Element>> = BTreeMap::new();
        for spec in layout.variables() {
            if spec.no_msq_save || layout.is_alias(&spec.name) {
                continue;
            }
            let Some(values) = tune.value(&spec.name) else {
                continue;
            };
            if values.is_empty() {
                continue;
            }
            let values = clamp_values(spec, values);
            let text = match load::format_values(spec, definition, &values) {
                Ok(text) => text,
                Err(error) => {
                    tracing::debug!(
                        name = %spec.name,
                        %error,
                        "the scaffold left this value out"
                    );
                    continue;
                }
            };

            let tag = match spec.ini_page() {
                Some(_) => EntryKind::Constant,
                None => EntryKind::PcVariable,
            };
            let element = entry_element(spec, is_list(definition, &spec.name), tag, &text);

            match spec.ini_page() {
                Some(page) => pages.entry(page.to_msq().get()).or_default().push(element),
                None => pc_entries.push(element),
            }
        }

        let mut root = Element::new("msq", &[("xmlns", SCAFFOLD_NAMESPACE)]);
        push_child(
            &mut root.children,
            1,
            Element::self_closing("bibliography", &[("author", "Ox Tuner")]),
        );
        push_child(&mut root.children, 1, version_element(definition));

        let pc_page = if pc_entries.is_empty() {
            Element::self_closing("page", &[])
        } else {
            page_element(None, None, pc_entries)
        };
        push_child(&mut root.children, 1, pc_page);

        for page in &definition.pages {
            let Some(ini) = IniPage::new(page.number) else {
                continue;
            };
            let number = ini.to_msq().get();
            let entries = pages.remove(&number).unwrap_or_default();
            let size = page.size.to_string();
            let numbered = if entries.is_empty() {
                Element::self_closing("page", &[("number", &number.to_string()), ("size", &size)])
            } else {
                page_element(Some(&number.to_string()), Some(&size), entries)
            };
            push_child(&mut root.children, 1, numbered);
        }

        if let Some(leftover) = pages.keys().next() {
            return Err(MsqError::UnknownPage { page: *leftover });
        }

        push_child(&mut root.children, 1, settings_element(settings));
        push_child(
            &mut root.children,
            1,
            Element::self_closing("userComments", &[]),
        );

        let document = Document {
            declaration: Some("xml version=\"1.0\" encoding=\"ISO-8859-1\"".to_owned()),
            nodes: vec![
                Node::Text("\n".to_owned()),
                Node::Element(root),
                Node::Text("\n".to_owned()),
            ],
        };
        let scaffold = Self { document };
        Self::check_defaults(&scaffold, definition)?;

        Ok(scaffold)
    }

    /// Refuses a scaffold whose written values hold two for one byte.
    ///
    /// The clamp and the user-unit rounding are part of what lands on disk, and
    /// either can move one peer and not the other after the settle. What the
    /// file says is what a project opens on, so that is what is judged: a tune
    /// that would open holding two values for one byte is refused here instead
    /// of being created broken.
    fn check_defaults(scaffold: &Self, definition: &Definition) -> Result<(), MsqError> {
        let bytes = scaffold.save()?;
        let reparsed = Self::parse(&bytes)?;
        let loaded = load::load(&reparsed, definition);

        for page in loaded.layout.pages() {
            let conflicts = loaded.layout.value_conflicts(page.page, &loaded.tune);
            if let Some(conflict) = conflicts.into_iter().next() {
                return Err(MsqError::ConflictingDefaults { conflict });
            }
        }

        Ok(())
    }

    /// Parses `.msq` bytes (ISO-8859-1) into the document model.
    ///
    /// # Errors
    ///
    /// Returns [`MsqError`] when the XML is malformed or the root is not
    /// `<msq>`.
    pub fn parse(bytes: &[u8]) -> Result<Self, MsqError> {
        let source = oxtuner_encoding::decode_latin1(bytes);
        let document = Document::parse(&source)?;
        let root = document.root().ok_or_else(|| MsqError::RootIsNotMsq {
            found: String::new(),
        })?;
        if root.name != "msq" {
            return Err(MsqError::RootIsNotMsq {
                found: root.name.clone(),
            });
        }
        Ok(Self { document })
    }

    /// Serializes back to ISO-8859-1 bytes.
    ///
    /// # Errors
    ///
    /// Returns [`MsqError::Encoding`] when the text has a character outside
    /// ISO-8859-1.
    pub fn save(&self) -> Result<Vec<u8>, MsqError> {
        Ok(oxtuner_encoding::encode_latin1(&self.document.serialize())?)
    }

    /// The parsed document, for callers that need raw access (tests, tools).
    pub fn document(&self) -> &Document {
        &self.document
    }

    fn root(&self) -> Option<&Element> {
        self.document.root()
    }

    fn root_mut(&mut self) -> Option<&mut Element> {
        self.document.root_mut()
    }

    /// The `<versionInfo>` header, when present.
    pub fn version_info(&self) -> Option<VersionInfo> {
        let version = self.root()?.find_child("versionInfo")?;
        Some(VersionInfo {
            file_format: version.attr("fileFormat").unwrap_or_default().to_owned(),
            firmware_info: version.attr("firmwareInfo").unwrap_or_default().to_owned(),
            n_pages: version
                .attr("nPages")
                .and_then(|value| value.trim().parse().ok()),
            signature: version.attr("signature").unwrap_or_default().to_owned(),
        })
    }

    /// Every `<page>` element, in document order.
    fn page_elements(&self) -> impl Iterator<Item = &Element> {
        self.root()
            .into_iter()
            .flat_map(|root| root.find_children("page"))
    }

    /// The numbered calibration pages, in `number` order.
    ///
    /// A `<page>` without a numeric `number` attribute is not a calibration
    /// page and is skipped here; [`Self::pc_page_count`] reports those.
    pub fn pages(&self) -> Vec<PageHeader> {
        let mut pages: Vec<PageHeader> = self
            .page_elements()
            .filter_map(|element| {
                let page = element.attr("number")?.trim().parse().ok()?;
                Some(PageHeader {
                    page: MsqPage::new(page),
                    size: element
                        .attr("size")
                        .and_then(|value| value.trim().parse().ok()),
                })
            })
            .collect();
        pages.sort_by_key(|page| page.page);
        pages
    }

    /// Number of PC-variable pages (normally exactly one).
    ///
    /// A page is the PC page when it has no `number` attribute or a negative
    /// one: an unnumbered page and `number="-1"` name the same thing, wherever
    /// the writer placed it.
    pub fn pc_page_count(&self) -> usize {
        self.page_elements()
            .filter(|element| page_address(element) == Some(EntryPage::Pc))
            .count()
    }

    /// Every value entry: the PC pages first, then numbered pages in file order.
    pub fn entries(&self) -> Vec<Entry> {
        let mut entries = Vec::new();
        for page in self.page_elements() {
            // A malformed number is not silently treated as page 0; the
            // consistency check reports it as a blocker.
            let Some(address) = page_address(page) else {
                continue;
            };
            for element in page.child_elements() {
                let kind = match element.name.as_str() {
                    "constant" => EntryKind::Constant,
                    "pcVariable" => EntryKind::PcVariable,
                    _ => continue,
                };
                entries.push(Entry {
                    page: address,
                    kind,
                    name: element.attr("name").unwrap_or_default().to_owned(),
                    value: element.trimmed_text(),
                    cols: attribute_usize(element, "cols"),
                    rows: attribute_usize(element, "rows"),
                    digits: attribute_usize(element, "digits")
                        .and_then(|digits| u8::try_from(digits).ok()),
                });
            }
        }
        entries
    }

    /// Every `<pageData>` block, as `(0-based page number, decoded bytes)`.
    ///
    /// Some writers store the whole page as continuous hex beside the decoded
    /// `<constant>` values. The constants stay authoritative; this exists so
    /// the load gate can check that the two agree. A block whose text is not
    /// valid hex is skipped.
    pub fn page_data(&self) -> Vec<(u32, Vec<u8>)> {
        let Some(root) = self.root() else {
            return Vec::new();
        };

        root.find_children("pageData")
            .filter_map(|element| {
                let page = element.attr("page")?.trim().parse::<u32>().ok()?;
                let bytes = decode_hex(&element.trimmed_text())?;
                Some((page, bytes))
            })
            .collect()
    }

    /// The `<settings>` snapshot as `(name, value)` pairs, in file order.
    pub fn settings(&self) -> Vec<(String, String)> {
        let Some(settings) = self.root().and_then(|root| root.find_child("settings")) else {
            return Vec::new();
        };
        settings
            .find_children("setting")
            .map(|setting| {
                (
                    setting.attr("name").unwrap_or_default().to_owned(),
                    setting.attr("value").unwrap_or_default().to_owned(),
                )
            })
            .collect()
    }

    /// Updates the `<settings>` snapshot to mirror the definition's flags.
    ///
    /// An entry whose name or value maps to a flag the definition tests is kept
    /// only when that flag is active; entries this build cannot map
    /// (`TunerStudio` setting-group choices) are preserved. Flags that became
    /// active but have no entry are appended, so a standalone reopen sees the
    /// same definition.
    ///
    /// A document that already agrees with the context is left untouched:
    /// `TunerStudio` writes a disabled boolean as an explicit `enablehardware_test_OFF`
    /// entry, and a save with no settings change must not delete it (the file
    /// has to stay byte-for-byte).
    ///
    /// A setting-group choice puts the flag in the entry's `value`
    /// (`pressure_units` → `pressure_bar`), while the `name` is the group, so
    /// both halves are read. Without that, a tune saved with a non-default
    /// group would gain a second entry for the option name on its first save.
    ///
    /// # Errors
    ///
    /// Returns [`MsqError`] when the document cannot represent a setting
    /// update.
    pub fn set_settings(
        &mut self,
        settings: &SettingsContext,
        tested: &BTreeSet<String>,
    ) -> Result<(), MsqError> {
        let Some(root) = self.root_mut() else {
            return Ok(());
        };
        let Some(element) = find_child_mut(root, "settings") else {
            return Ok(());
        };

        let mut documented: BTreeMap<String, bool> = BTreeMap::new();
        for setting in element.find_children("setting") {
            for (flag, state) in documented_flags(setting) {
                documented.insert(flag, state);
            }
        }
        let agrees = tested.iter().all(|flag| {
            // A flag that is neither documented nor active matches the absent
            // entry; only a disagreement needs a rewrite.
            documented.get(flag).copied().unwrap_or(false) == settings.is_active(flag)
        });
        if agrees {
            return Ok(());
        }

        let mut present: BTreeSet<String> = BTreeSet::new();
        let children = std::mem::take(&mut element.children);
        let mut kept = Vec::with_capacity(children.len());
        let mut removed = false;
        for node in children {
            let keep = match &node {
                Node::Element(setting) if setting.name == "setting" => {
                    // The name is the group, the value the selected option: both
                    // may be the tested flag, and a disagreement on either one
                    // drops the entry.
                    let documented = documented_flags(setting);
                    let agrees = documented
                        .iter()
                        .filter(|(flag, _)| tested.contains(flag))
                        .all(|(flag, state)| *state == settings.is_active(flag));
                    if agrees {
                        for (flag, _) in documented.iter().filter(|(flag, _)| tested.contains(flag))
                        {
                            present.insert(flag.clone());
                        }
                    }
                    agrees
                }
                _ => true,
            };
            if keep {
                kept.push(node);
            } else {
                removed = true;
                if matches!(kept.last(), Some(Node::Text(text)) if text.trim().is_empty()) {
                    // Drop the indentation that belonged to the removed line, so
                    // a removal does not leave a blank line behind.
                    kept.pop();
                }
            }
        }

        let missing: Vec<String> = settings
            .active_flags()
            .into_iter()
            .filter(|flag| {
                // `tested` and `present` are lowercase match keys; the flag
                // itself keeps the definition's spelling for the file.
                let key = flag.to_ascii_lowercase();
                tested.contains(&key) && !present.contains(&key)
            })
            .collect();
        let appended = !missing.is_empty();
        if appended {
            // Reuse the closing indentation as the boundary for new entries.
            if matches!(kept.last(), Some(Node::Text(text)) if text.trim().is_empty()) {
                kept.pop();
            }
            for flag in missing {
                kept.push(Node::Text("\n        ".to_owned()));
                kept.push(Node::Element(Element::self_closing(
                    "setting",
                    &[("name", &flag), ("value", &flag)],
                )));
            }
            kept.push(Node::Text("\n    ".to_owned()));
        }

        if !removed && !appended {
            // Nothing was dropped or appended; keep the element exactly as it
            // was written, including `<settings/>`.
            element.children = kept;
            return Ok(());
        }
        // A self-closing `<settings/>` has to be opened, or the serializer would
        // emit it unchanged and lose the new entries.
        element.open();
        element.children = kept;
        Ok(())
    }

    /// Replaces one entry's text, preserving everything else in the document.
    ///
    /// The caller supplies the final text (already formatted and *decoded*);
    /// XML escaping happens here.
    ///
    /// # Errors
    ///
    /// Returns [`MsqError::EntryNotFound`] when the page or entry is missing.
    pub fn set_entry_text(
        &mut self,
        page: EntryPage,
        kind: EntryKind,
        name: &str,
        text: &str,
    ) -> Result<(), MsqError> {
        let Some(root) = self.root_mut() else {
            return Err(MsqError::EntryNotFound {
                page,
                name: name.to_owned(),
            });
        };

        for page_element in root.children.iter_mut().filter_map(|node| match node {
            Node::Element(element) if element.name == "page" => Some(element),
            _ => None,
        }) {
            let Some(address) = page_address(page_element) else {
                continue;
            };
            if address != page {
                continue;
            }
            for entry in page_element
                .children
                .iter_mut()
                .filter_map(|node| match node {
                    Node::Element(element) if element.name == kind.element() => Some(element),
                    _ => None,
                })
            {
                // Names are keyed case-insensitively everywhere else (the tune
                // layout lowercases them), so a file that spells a name with a
                // different case must be updated in place, not duplicated.
                if entry
                    .attr("name")
                    .is_some_and(|written| written.eq_ignore_ascii_case(name))
                {
                    entry.set_text(text);
                    return Ok(());
                }
            }
        }

        Err(MsqError::EntryNotFound {
            page,
            name: name.to_owned(),
        })
    }

    /// Whether the document carries one entry.
    #[must_use]
    pub fn has_entry(&self, page: EntryPage, kind: EntryKind, name: &str) -> bool {
        let Some(root) = self.root() else {
            return false;
        };

        root.children
            .iter()
            .filter_map(|node| match node {
                Node::Element(element) if element.name == "page" => Some(element),
                _ => None,
            })
            .filter(|element| page_address(element) == Some(page))
            .flat_map(|element| element.children.iter())
            .filter_map(|node| match node {
                Node::Element(element) if element.name == kind.element() => Some(element),
                _ => None,
            })
            .any(|entry| {
                entry
                    .attr("name")
                    .is_some_and(|written| written.eq_ignore_ascii_case(name))
            })
    }

    /// Removes one entry from the document, when it is there.
    ///
    /// A constant whose bytes another constant covers is a view and must not
    /// survive in a saved file: another tool would apply it after the owner and
    /// read a different tune. A document without the entry is not an error.
    pub fn remove_entry(&mut self, page: EntryPage, kind: EntryKind, name: &str) {
        let Some(root) = self.root_mut() else {
            return;
        };

        for page_element in root.children.iter_mut().filter_map(|node| match node {
            Node::Element(element) if element.name == "page" => Some(element),
            _ => None,
        }) {
            if page_address(page_element) != Some(page) {
                continue;
            }

            page_element.children.retain(|node| match node {
                Node::Element(entry) if entry.name == kind.element() => !entry
                    .attr("name")
                    .is_some_and(|written| written.eq_ignore_ascii_case(name)),
                _ => true,
            });

            return;
        }
    }

    /// Like [`Self::set_entry_text`], but appends a new entry when the file
    /// has none.
    ///
    /// A definition entry the file omitted is a warning, not an error: the
    /// tune opened with the default, the user may edit it, and saving must
    /// write that edit instead of pretending it does not exist. The new
    /// element carries only `name`; `TunerStudio` reads the rest from the INI.
    ///
    /// A page element the document lacks stays an error. Page order and
    /// attributes belong to the writer, and a synthesized `<page>` could
    /// disagree with the file's own layout, so the caller gets a clean
    /// [`MsqError::EntryNotFound`] instead of a guessed page.
    ///
    /// # Errors
    ///
    /// Returns [`MsqError`] when the document cannot gain the element.
    pub fn put_entry_text(
        &mut self,
        page: EntryPage,
        kind: EntryKind,
        name: &str,
        text: &str,
    ) -> Result<(), MsqError> {
        if self.set_entry_text(page, kind, name, text).is_ok() {
            return Ok(());
        }

        let tag = kind.element();
        let Some(root) = self.root_mut() else {
            return Err(MsqError::EntryNotFound {
                page,
                name: name.to_owned(),
            });
        };

        let target = root.children.iter_mut().find_map(|node| match node {
            Node::Element(element) if element.name == "page" => {
                let address = page_address(element);
                (address == Some(page)).then_some(element)
            }
            _ => None,
        });

        let Some(page_element) = target else {
            return Err(MsqError::EntryNotFound {
                page,
                name: name.to_owned(),
            });
        };

        let mut element = Element::new(tag, &[("name", name)]);
        element.set_text(text);
        // A self-closing `<page …/>` must be opened first, or the serializer
        // would keep emitting it unchanged and the new entry would be lost.
        page_element.open();
        page_element
            .children
            .push(Node::Text("\n        ".to_owned()));
        page_element.children.push(Node::Element(element));
        page_element.children.push(Node::Text("\n    ".to_owned()));
        Ok(())
    }
}

/// A `<constant>` or `<pcVariable>` element for one formatted value.
///
/// Arrays carry their `cols`/`rows` shape, the same attributes the reference
/// writer stores; scalars and labels need only their name.
fn entry_element(spec: &VariableSpec, is_list: bool, kind: EntryKind, text: &str) -> Element {
    let cols_text;
    let rows_text;
    let mut attrs: Vec<(&str, &str)> = vec![("name", spec.name.as_str())];
    if let EntryShape::Array { cols, rows } = spec.shape {
        // A one-dimensional list is written as one column of N rows, the way
        // the reference writer stores it. A `[Nx1]` table keeps its declared
        // shape: only the definition's own `[N]` form is a list.
        let (cols, rows) = if is_list && rows == 1 && cols > 1 {
            (1, cols)
        } else {
            (cols, rows)
        };
        cols_text = cols.to_string();
        rows_text = rows.to_string();
        attrs.push(("cols", cols_text.as_str()));
        attrs.push(("rows", rows_text.as_str()));
    }

    let mut element = Element::new(kind.element(), &attrs);
    element.set_text(text);
    element
}

/// A `<page>` element with its entries, indented one level deeper.
fn page_element(number: Option<&str>, size: Option<&str>, entries: Vec<Element>) -> Element {
    let mut attrs: Vec<(&str, &str)> = Vec::new();
    if let Some(number) = number {
        attrs.push(("number", number));
    }
    if let Some(size) = size {
        attrs.push(("size", size));
    }

    let mut page = Element::new("page", &attrs);
    for entry in entries {
        push_child(&mut page.children, 2, entry);
    }
    page.children.push(Node::Text("\n    ".to_owned()));
    page
}

/// The `<versionInfo>` header for a scaffolded tune.
///
/// `firmwareInfo` carries the signature: the INI's `versionInfo` key is the
/// interrogate command (Speeduino answers `"S"` with the signature), not
/// firmware text, so only the signature describes the firmware.
fn version_element(definition: &Definition) -> Element {
    let pages = definition.pages.len().to_string();
    Element::self_closing(
        "versionInfo",
        &[
            ("fileFormat", SCAFFOLD_FILE_FORMAT),
            ("firmwareInfo", definition.signature.as_str()),
            ("nPages", &pages),
            ("signature", definition.signature.as_str()),
        ],
    )
}

/// The `<settings>` snapshot for the active flags.
fn settings_element(settings: &SettingsContext) -> Element {
    let active = settings.active_flags();
    if active.is_empty() {
        return Element::self_closing("settings", &[]);
    }

    let mut element = Element::self_closing("settings", &[]);
    element.open();
    for flag in active {
        let flag = flag.as_str();
        push_child(
            &mut element.children,
            2,
            Element::self_closing("setting", &[("name", flag), ("value", flag)]),
        );
    }
    element.children.push(Node::Text("\n    ".to_owned()));
    element
}

/// Appends an indented child element to a parent's children.
fn push_child(children: &mut Vec<Node>, depth: usize, element: Element) {
    children.push(Node::Text(format!("\n{}", "    ".repeat(depth))));
    children.push(Node::Element(element));
}

fn attribute_usize(element: &Element, name: &str) -> Option<usize> {
    element.attr(name)?.trim().parse().ok()
}

/// Decodes a continuous hex string, ignoring whitespace.
fn decode_hex(text: &str) -> Option<Vec<u8>> {
    let compact: String = text
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect();
    if compact.is_empty() || !compact.len().is_multiple_of(2) {
        return None;
    }

    let mut bytes = Vec::with_capacity(compact.len() / 2);
    let mut index = 0;
    while index < compact.len() {
        let pair = compact.get(index..index + 2)?;
        bytes.push(u8::from_str_radix(pair, 16).ok()?);
        index += 2;
    }

    Some(bytes)
}

/// The flags one `<setting>` entry documents, with the state each implies.
///
/// `TunerStudio` writes a boolean as name and value (`enablehardware_test_OFF`),
/// and a setting-group choice as the group in `name` and the selected option in
/// `value` (`pressure_units` → `pressure_bar`). Both halves are flags the
/// definition can test in `#if`, so both are returned, deduplicated and
/// lowercased to match `SettingsContext`.
fn documented_flags(setting: &Element) -> Vec<(String, bool)> {
    let mut flags_documented: Vec<(String, bool)> = Vec::with_capacity(2);
    for text in [setting.attr("name"), setting.attr("value")] {
        let Some(text) = text else { continue };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        let (flag, state) = flags::state(text);
        let key = flag.to_ascii_lowercase();
        if flags_documented.iter().any(|(seen, _)| *seen == key) {
            continue;
        }
        flags_documented.push((key, state));
    }
    flags_documented
}

fn find_child_mut<'a>(element: &'a mut Element, name: &str) -> Option<&'a mut Element> {
    element.children.iter_mut().find_map(|node| match node {
        Node::Element(child) if child.name == name => Some(child),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "unit tests may unwrap; a failed call is a failed test"
    )]
    use super::*;
    use oxtuner_ini::parse_str;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="ISO-8859-1"?>
<msq xmlns="http://www.msefi.com/:msq">
    <bibliography author="TunerStudio" tuneComment=""/>
    <versionInfo fileFormat="5.0" firmwareInfo="Speeduino+2025.01.3" nPages="2" signature="speeduino 202501"/>
    <page>
        <pcVariable digits="0" name="rpmhigh" units="rpm">12000.0</pcVariable>
        <pcVariable digits="0" name="rpmwarn" units="rpm">10500.0</pcVariable>
    </page>
    <page number="0" size="128">
        <constant digits="1" name="reqFuel" units="ms">15.4</constant>
        <constant name="aeMode">"TPS"</constant>
        <constant cols="1" digits="0" name="wueRates" rows="3" units="%">
         145.0 
         135.0 
         100.0 
      </constant>
        <!-- unknown to build 0.1, must survive -->
        <futureThing mode="x"><nested a="1"/></futureThing>
    </page>
    <page number="1" size="288">
        <constant digits="0" name="divider" units="">1.0</constant>
    </page>
    <settings Comment="written by TunerStudio">
        <setting name="enablehardware_test_OFF" value="enablehardware_test_OFF"/>
        <setting name="CELSIUS" value="CELSIUS"/>
        <setting name="resetcontrol_standard" value="resetcontrol_standard"/>
    </settings>
</msq>
"#;

    fn sample() -> Msq {
        Msq::parse(SAMPLE.as_bytes()).expect("sample parses")
    }

    #[test]
    fn the_header_and_pages_are_read_from_attributes_not_order() {
        let msq = sample();
        let info = msq.version_info().unwrap();
        assert_eq!(info.signature, "speeduino 202501");
        assert_eq!(info.n_pages, Some(2));

        assert_eq!(msq.pc_page_count(), 1);
        assert_eq!(
            msq.pages(),
            vec![
                PageHeader {
                    page: MsqPage::new(0),
                    size: Some(128)
                },
                PageHeader {
                    page: MsqPage::new(1),
                    size: Some(288)
                },
            ]
        );
    }

    #[test]
    fn a_negative_page_number_is_the_pc_page() {
        let source = "<msq><versionInfo signature=\"s\"/><page number=\"-1\"><pcVariable name=\"rpmhigh\">12000.0</pcVariable></page><page number=\"0\" size=\"128\"/></msq>";
        let msq = Msq::parse(source.as_bytes()).expect("parses");

        assert_eq!(msq.pages().len(), 1, "only page 0 is a calibration page");
        assert_eq!(
            msq.pc_page_count(),
            1,
            "the numbered PC page counts as the PC page"
        );

        let entries = msq.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].page, EntryPage::Pc);
        assert_eq!(entries[0].name, "rpmhigh");
    }

    #[test]
    fn entries_keep_their_page_and_kind() {
        let msq = sample();
        let entries = msq.entries();

        assert_eq!(entries.len(), 6);
        assert_eq!(entries[0].page, EntryPage::Pc);
        assert_eq!(entries[0].kind, EntryKind::PcVariable);
        assert_eq!(entries[2].page, EntryPage::Numbered(MsqPage::new(0)));
        assert_eq!(entries[2].name, "reqFuel");
        assert_eq!(entries[3].value, "\"TPS\"");
        assert_eq!(entries[4].tokens(), ["145.0", "135.0", "100.0"]);
        assert_eq!(entries[4].rows, Some(3));
    }

    #[test]
    fn an_unmodified_save_is_byte_identical() {
        let msq = sample();
        assert_eq!(msq.save().unwrap(), SAMPLE.as_bytes());
    }

    #[test]
    fn page_data_blocks_decode_from_hex() {
        let source = r#"<msq>
    <versionInfo signature="s"/>
    <pageData page="0">00 ff 10</pageData>
    <pageData page="1">0x</pageData>
</msq>"#;
        let msq = Msq::parse(source.as_bytes()).expect("parses");

        assert_eq!(msq.page_data(), vec![(0, vec![0x00, 0xFF, 0x10])]);
    }

    #[test]
    fn editing_one_value_leaves_unknown_elements_and_comments_intact() {
        let mut msq = sample();
        msq.set_entry_text(
            EntryPage::Numbered(MsqPage::new(0)),
            EntryKind::Constant,
            "reqFuel",
            "16.2",
        )
        .unwrap();

        let saved = String::from_utf8(msq.save().unwrap()).unwrap();
        assert!(
            saved.contains("<constant digits=\"1\" name=\"reqFuel\" units=\"ms\">16.2</constant>")
        );
        assert!(saved.contains("<!-- unknown to build 0.1, must survive -->"));
        assert!(saved.contains("<futureThing mode=\"x\"><nested a=\"1\"/></futureThing>"));
        assert!(saved.contains("<bibliography author=\"TunerStudio\" tuneComment=\"\"/>"));
    }

    #[test]
    fn an_edit_of_a_missing_entry_is_an_error() {
        let mut msq = sample();
        assert_eq!(
            msq.set_entry_text(
                EntryPage::Numbered(MsqPage::new(0)),
                EntryKind::Constant,
                "missing",
                "1.0",
            ),
            Err(MsqError::EntryNotFound {
                page: EntryPage::Numbered(MsqPage::new(0)),
                name: "missing".to_owned(),
            })
        );
    }

    #[test]
    fn settings_mirror_the_active_flags_and_keep_unknown_entries() {
        let mut msq = sample();
        let mut context = SettingsContext::new();
        context.set_override("CELSIUS", false);
        context.set_override("LAMBDA", true);
        let tested: BTreeSet<String> = ["celsius", "lambda", "enablehardware_test"]
            .into_iter()
            .map(str::to_owned)
            .collect();

        msq.set_settings(&context, &tested).unwrap();
        let settings = msq.settings();

        assert!(
            settings
                .iter()
                .any(|(name, _)| name == "resetcontrol_standard"),
            "a setting group choice is not a flag and must survive"
        );
        assert!(
            settings
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("LAMBDA"))
        );
        assert!(!settings.iter().any(|(name, _)| name == "CELSIUS"));
        assert!(
            settings
                .iter()
                .any(|(name, _)| name == "enablehardware_test_OFF"),
            "an explicit OFF entry already mirrors an inactive flag"
        );

        // Turning the flag on replaces the explicit OFF entry with the bare name.
        let mut context = SettingsContext::new();
        context.set_override("enablehardware_test", true);
        msq.set_settings(&context, &tested).unwrap();
        let settings = msq.settings();
        assert!(
            settings
                .iter()
                .any(|(name, _)| name == "enablehardware_test")
        );
        assert!(
            !settings
                .iter()
                .any(|(name, _)| name == "enablehardware_test_OFF")
        );
    }

    #[test]
    fn a_snapshot_that_already_agrees_is_not_rewritten() {
        let source = SAMPLE.as_bytes();
        let mut msq = Msq::parse(source).expect("the sample parses");
        let mut context = SettingsContext::new();
        context.set_override("CELSIUS", true);
        context.set_override("enablehardware_test", false);
        let tested: BTreeSet<String> = ["celsius", "enablehardware_test"]
            .into_iter()
            .map(str::to_owned)
            .collect();

        msq.set_settings(&context, &tested).unwrap();
        assert_eq!(msq.save().unwrap(), source);
    }

    #[test]
    fn a_document_with_a_foreign_root_is_rejected() {
        assert_eq!(
            Msq::parse(b"<tune/>").unwrap_err(),
            MsqError::RootIsNotMsq {
                found: "tune".to_owned()
            }
        );
    }

    #[test]
    fn scaffold_emits_a_document_that_checks_clean() {
        let definition = scaffold_definition();
        let settings = SettingsContext::new();
        let msq = Msq::scaffold(&definition, &settings, None).expect("defaults scaffold");

        let info = msq.version_info().expect("the header is present");
        assert_eq!(info.file_format, "5.0");
        assert_eq!(info.signature, "speeduino 202501");
        assert_eq!(info.firmware_info, "speeduino 202501");
        assert_eq!(info.n_pages, Some(2));
        assert_eq!(msq.pages().len(), 2);
        assert_eq!(msq.pc_page_count(), 1);

        let report = crate::check(&msq, &definition);
        assert!(!report.is_read_only(), "{:?}", report.blockers);

        let entries = msq.entries();
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        for name in ["rpmhigh", "reqFuel", "aeMode", "note", "table", "b"] {
            assert!(names.contains(&name), "{name} is scaffolded");
        }

        let table = entries
            .iter()
            .find(|entry| entry.name == "table")
            .expect("the array is present");
        assert_eq!((table.cols, table.rows), (Some(2), Some(2)));

        // The document survives a save and re-parse.
        let bytes = msq.save().expect("the scaffold serializes");
        let reparsed = Msq::parse(&bytes).expect("the scaffold parses");
        assert_eq!(reparsed.version_info().expect("the header survives"), info);
    }

    #[test]
    fn a_page_seed_replaces_defaults_but_not_pc_variables() {
        // A project created from a controller carries the ECU's page image.
        // The page protocol has no PC variables, so those keep their defaults.
        let definition = scaffold_definition();
        let settings = SettingsContext::new();
        let mut seed = Tune::from_definition(&definition);
        seed.set_image_raw("reqFuel", 0, 200.0).expect("seeded");
        seed.set_image_raw("aeMode", 0, 1.0).expect("seeded");
        seed.set_image_text("note", "ECU").expect("seeded");
        seed.set_image_raw("table", 3, 9.0).expect("seeded");
        seed.set_image_raw("rpmhigh", 0, 12_345.0).expect("seeded");

        let msq = Msq::scaffold(&definition, &settings, Some(&seed)).expect("seeded scaffold");
        let bytes = msq.save().expect("the scaffold serializes");
        let reparsed = Msq::parse(&bytes).expect("the scaffold parses");
        let loaded = load(&reparsed, &definition);

        assert_eq!(
            loaded.tune.raw_at("reqFuel", 0).expect("it is there"),
            200.0
        );
        assert_eq!(loaded.tune.raw_at("aeMode", 0).expect("it is there"), 1.0);
        assert_eq!(loaded.tune.text("note").expect("it is there"), "ECU");
        assert_eq!(loaded.tune.raw_at("table", 3).expect("it is there"), 9.0);
        assert_eq!(
            loaded.tune.raw_at("rpmhigh", 0).expect("it is there"),
            0.0,
            "a PC variable has no page and keeps its definition default"
        );
    }

    #[test]
    fn a_scaffolded_tune_keeps_peers_over_one_byte_in_step() {
        // A definition can give two constants the same bytes and disagree with
        // itself about what those bytes should hold: `second` cannot read back
        // as zero inside its range, so its default is seven while `first` stays
        // at zero. A new project must not be born holding two values for one
        // byte, and it has to read its own save back without a complaint.
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
nPages = 1
pageSize = 8
page = 1
first = scalar, U08, 0, \"x\", 1, 0, 0, 255, 0
second = scalar, U08, 0, \"x\", 1, 0, 7, 255, 0
",
            &SettingsContext::new(),
        )
        .expect("parses");
        let settings = SettingsContext::new();

        let msq = Msq::scaffold(&definition, &settings, None).expect("defaults scaffold");
        let bytes = msq.save().expect("the scaffold serializes");
        let reparsed = Msq::parse(&bytes).expect("the scaffold parses");
        let loaded = load(&reparsed, &definition);

        assert!(
            loaded
                .layout
                .value_conflicts(IniPage::FIRST, &loaded.tune)
                .is_empty(),
            "a project must not start with two values over one byte: {:?}",
            loaded.layout.value_conflicts(IniPage::FIRST, &loaded.tune)
        );
    }

    #[test]
    fn a_scaffold_whose_peers_have_no_common_value_is_refused() {
        // `first` cannot store anything from 11 up and `second` cannot store
        // anything below 20, so no value fits both and a new tune would open
        // holding two of them for one byte.
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
nPages = 1
pageSize = 8
page = 1
first = scalar, U08, 0, \"x\", 1, 0, 0, 10, 0
second = scalar, U08, 0, \"x\", 1, 0, 20, 255, 0
",
            &SettingsContext::new(),
        )
        .expect("parses");

        let error = Msq::scaffold(&definition, &SettingsContext::new(), None)
            .expect_err("no value fits both ranges");

        assert!(
            matches!(error, MsqError::ConflictingDefaults { .. }),
            "the refusal names the pair: {error}"
        );
    }

    #[test]
    fn a_scaffold_whose_peers_round_apart_is_refused() {
        // Both peers settle onto seven and both can store it, but `second`
        // writes it in user units as 0.07 and rounds that to one digit. The
        // file then says 0.1, which reads back as ten next to `first`'s seven.
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
nPages = 1
pageSize = 8
page = 1
first = scalar, U08, 0, \"x\", 1, 0, 7, 255, 0
second = scalar, U08, 0, \"y\", 0.01, 0, 0, 255, 1
",
            &SettingsContext::new(),
        )
        .expect("parses");

        let error = Msq::scaffold(&definition, &SettingsContext::new(), None)
            .expect_err("the rounding pulls the two apart");

        assert!(
            matches!(error, MsqError::ConflictingDefaults { .. }),
            "what lands on disk is what is judged: {error}"
        );
    }

    #[test]
    fn a_scaffold_keeps_the_values_of_a_page_it_cannot_rebuild() {
        // `wide` runs 0..4 and `straddle` runs 2..6, so no value owns what they
        // share and the page cannot go through the codec. A scaffold writes the
        // defaults as they are rather than settling one over the other.
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
nPages = 1
pageSize = 16
page = 1
wide = array, U08, 0, [4], \"\", 1, 0, 0, 255, 0
straddle = array, U08, 2, [4], \"\", 1, 0, 7, 255, 0
",
            &SettingsContext::new(),
        )
        .expect("parses");

        let msq =
            Msq::scaffold(&definition, &SettingsContext::new(), None).expect("defaults scaffold");
        let bytes = msq.save().expect("the scaffold serializes");
        let reparsed = Msq::parse(&bytes).expect("the scaffold parses");
        let loaded = load(&reparsed, &definition);

        for cell in 0..4 {
            assert_eq!(loaded.tune.raw_at("wide", cell).expect("it is there"), 0.0);
            assert_eq!(
                loaded.tune.raw_at("straddle", cell).expect("it is there"),
                7.0,
                "cell {cell} must not be written over by a rebuild"
            );
        }
    }

    #[test]
    fn a_scaffold_keeps_a_text_page_it_cannot_rebuild() {
        // The codec reads `note` as its whole eight bytes, so a rebuild would
        // hand the middle of it to the number underneath and read it back as
        // text. The scaffold writes both as they are instead.
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
nPages = 1
pageSize = 16
page = 1
note = string, ASCII, 8
inside = scalar, U08, 4, \"x\", 1, 0, 7, 255, 0
",
            &SettingsContext::new(),
        )
        .expect("parses");

        let msq =
            Msq::scaffold(&definition, &SettingsContext::new(), None).expect("defaults scaffold");
        let bytes = msq.save().expect("the scaffold serializes");
        let reparsed = Msq::parse(&bytes).expect("the scaffold parses");
        let loaded = load(&reparsed, &definition);

        assert_eq!(loaded.tune.text("note").expect("it is there"), "");
        assert_eq!(loaded.tune.raw_at("inside", 0).expect("it is there"), 7.0);
    }

    #[test]
    fn a_scaffolded_view_follows_its_owner_on_a_text_page() {
        // `view` reads `owner`'s second byte, and `note` is a text field over
        // the owner's remaining bytes, so the page cannot go through the codec.
        // A scaffold never writes a view, so opening the file back has to derive
        // it from its owner rather than leave it on its definition default.
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
nPages = 1
pageSize = 16
page = 1
owner = array, U08, 0, [4], \"\", 1, 0, 7, 255, 0
view = scalar, U08, 1, \"\", 1, 0, 0, 255, 0
note = string, ASCII, 8
",
            &SettingsContext::new(),
        )
        .expect("parses");

        let msq =
            Msq::scaffold(&definition, &SettingsContext::new(), None).expect("defaults scaffold");
        let bytes = msq.save().expect("the scaffold serializes");
        let reparsed = Msq::parse(&bytes).expect("the scaffold parses");
        let loaded = load(&reparsed, &definition);
        assert!(
            !loaded.layout.page_can_be_rebuilt(IniPage::FIRST),
            "this is the derive fallback only when the page is refused"
        );

        assert_eq!(
            loaded.tune.raw_at("view", 0).expect("it is there"),
            7.0,
            "the view reads the byte its owner holds when the file is opened back"
        );
    }

    fn scaffold_definition() -> Definition {
        let text = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 2
pageSize = 128, 288
page = 1
reqFuel = scalar, U08, 0, "ms", 0.1, 0, 0, 25.5, 1
aeMode = bits, U08, 1, [0:7], "TPS", "MAP", "Alpha-N"
note = string, ASCII, 8
table = array, U08, 4, [2x2], "", 1, 0, 0, 255, 0
page = 2
b = scalar, U08, 0, "", 1, 0, 0, 255, 0

[PcVariables]
rpmhigh = scalar, U16, 0, "rpm", 1, 0, 0, 20000, 0
"#;
        oxtuner_ini::parse_str_with_includes(
            text,
            &SettingsContext::new(),
            &|_: oxtuner_ini::IncludeRequest<'_>| None,
        )
        .expect("the fixture parses")
    }

    #[test]
    fn latin1_bytes_survive_the_round_trip() {
        let source: &[u8] =
            b"<msq>\n  <bibliography author=\"Jos\xe9\"/>\n  <versionInfo signature=\"s\"/>\n</msq>";
        let msq = Msq::parse(source).expect("latin-1 parses");
        assert_eq!(msq.save().unwrap(), source);
    }

    #[test]
    fn an_edit_of_a_self_closing_entry_is_written() {
        let source = "<msq><versionInfo signature=\"s\"/><page number=\"0\" size=\"64\"><constant name=\"x\"/></page></msq>";
        let mut msq = Msq::parse(source.as_bytes()).expect("parses");

        msq.set_entry_text(
            EntryPage::Numbered(MsqPage::new(0)),
            EntryKind::Constant,
            "x",
            "2.5",
        )
        .expect("the entry exists");

        let saved = String::from_utf8(msq.save().unwrap()).unwrap();
        assert!(
            saved.contains("<constant name=\"x\">2.5</constant>"),
            "the edit must not be swallowed by `<constant name=\"x\"/>`: {saved}"
        );
    }

    #[test]
    fn a_missing_entry_is_appended_to_a_self_closing_page() {
        let source = "<msq><versionInfo signature=\"s\"/><page number=\"0\" size=\"64\"/></msq>";
        let mut msq = Msq::parse(source.as_bytes()).expect("parses");

        msq.put_entry_text(
            EntryPage::Numbered(MsqPage::new(0)),
            EntryKind::Constant,
            "x",
            "1.5",
        )
        .expect("the page exists");

        let saved = String::from_utf8(msq.save().unwrap()).unwrap();
        assert!(
            saved.contains("<constant name=\"x\">1.5</constant>"),
            "a self-closing page must be opened before it gains an entry: {saved}"
        );
        assert!(
            !saved.contains("<page number=\"0\" size=\"64\"/>"),
            "{saved}"
        );
    }

    #[test]
    fn a_self_closing_settings_block_receives_the_missing_entries() {
        let source = "<msq><versionInfo signature=\"s\"/><settings/></msq>";
        let mut msq = Msq::parse(source.as_bytes()).expect("parses");
        let mut context = SettingsContext::new();
        context.set_override("CELSIUS", true);
        let tested: BTreeSet<String> = ["celsius".to_owned()].into_iter().collect();

        msq.set_settings(&context, &tested)
            .expect("settings update");

        let saved = String::from_utf8(msq.save().unwrap()).unwrap();
        assert!(saved.contains("<setting name=\"CELSIUS\""), "{saved}");
        assert!(!saved.contains("<settings/>"), "{saved}");
    }

    #[test]
    fn a_setting_group_choice_is_not_duplicated_on_a_no_op_save() {
        let source = r#"<msq>
    <versionInfo signature="s"/>
    <settings>
        <setting name="pressure_units" value="pressure_bar"/>
    </settings>
</msq>"#;
        let mut msq = Msq::parse(source.as_bytes()).expect("parses");
        // `Project::settings_overrides` records both halves of the entry.
        let mut context = SettingsContext::new();
        context.set_override("pressure_units", true);
        context.set_override("pressure_bar", true);
        let tested: BTreeSet<String> = ["pressure_bar".to_owned()].into_iter().collect();

        msq.set_settings(&context, &tested)
            .expect("settings update");

        assert_eq!(
            msq.save().unwrap(),
            source.as_bytes(),
            "the option lives in the entry's value; it must not be appended again"
        );
        assert_eq!(msq.settings().len(), 1);
    }

    #[test]
    fn an_entry_name_is_matched_ignoring_case() {
        let source = "<msq><versionInfo signature=\"s\"/><page number=\"0\" size=\"64\"><constant name=\"REQFUEL\">1.0</constant></page></msq>";
        let mut msq = Msq::parse(source.as_bytes()).expect("parses");

        msq.set_entry_text(
            EntryPage::Numbered(MsqPage::new(0)),
            EntryKind::Constant,
            "reqFuel",
            "2.0",
        )
        .expect("the name is the same, whatever the case");

        let saved = String::from_utf8(msq.save().unwrap()).unwrap();
        assert_eq!(saved.matches("<constant").count(), 1, "{saved}");
        assert!(
            saved.contains("<constant name=\"REQFUEL\">2.0</constant>"),
            "{saved}"
        );
    }

    #[test]
    fn a_setting_name_maps_to_its_flag() {
        assert_eq!(
            flags::state("enablehardware_test_OFF"),
            ("enablehardware_test".to_owned(), false)
        );
        assert_eq!(
            flags::state("enablehardware_test_Off"),
            ("enablehardware_test".to_owned(), false),
            "a mixed-case suffix must read the same as the canonical one"
        );
        assert_eq!(
            flags::state("enablehardware_test_ON"),
            ("enablehardware_test".to_owned(), true)
        );
        assert_eq!(flags::state("CELSIUS"), ("CELSIUS".to_owned(), true));
    }
}
