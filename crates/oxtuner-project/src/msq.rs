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

use oxtuner_ini::{Definition, SettingsContext};
use oxtuner_tune::{EntryShape, IniPage, MsqPage, Tune, VariableSpec};

use crate::flags;
use crate::load::{self, FileValueError};
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
        }
    }
}

impl std::error::Error for MsqError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Xml(error) => Some(error),
            Self::Encoding(error) => Some(error),
            Self::RootIsNotMsq { .. } | Self::EntryNotFound { .. } => None,
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

impl Msq {
    /// Builds a new tune document with every value at its definition default.
    ///
    /// The document mirrors what the reference writer produces: an unnumbered
    /// PC page first, one numbered page per definition page carrying the
    /// definition's page sizes, and a `<settings>` snapshot of the active
    /// flags. Variables the definition marks `noMsqSave` are left out, the
    /// same rule the save path applies.
    ///
    /// A variable whose value cannot be written from a tune with only defaults
    /// is left out as well, exactly once, and the open that follows reports it
    /// as a missing value. Speeduino's `iacCLminValue` is the known case: its
    /// `scale` is an expression over other values, and a tune with only
    /// defaults has nothing to resolve it from.
    ///
    /// This is crate-private: project creation is the only caller.
    pub(crate) fn scaffold(
        definition: &Definition,
        settings: &SettingsContext,
    ) -> Result<Self, FileValueError> {
        let tune = Tune::from_definition(definition);
        let layout = Arc::clone(tune.layout());

        let mut pc_entries: Vec<Element> = Vec::new();
        let mut pages: BTreeMap<u16, Vec<Element>> = BTreeMap::new();
        for spec in layout.variables() {
            if spec.no_msq_save {
                continue;
            }
            let Some(values) = tune.value(&spec.name) else {
                continue;
            };
            if values.is_empty() {
                continue;
            }
            let Ok(text) = load::format_values(spec, definition, values) else {
                continue;
            };

            let tag = match spec.ini_page() {
                Some(_) => EntryKind::Constant,
                None => EntryKind::PcVariable,
            };
            let element = entry_element(spec, tag, &text);

            match spec.ini_page() {
                Some(page) => pages.entry(page.to_msq().get()).or_default().push(element),
                None => pc_entries.push(element),
            }
        }

        let mut root = Element::new("msq", &[("xmlns", SCAFFOLD_NAMESPACE)]);
        push_child(
            &mut root.children,
            1,
            Element::self_closing("bibliography", &[("author", "OxTuner")]),
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
            return Err(FileValueError::UnknownPage { page: *leftover });
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
        Ok(Self { document })
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
            .filter(|flag| tested.contains(flag) && !present.contains(flag))
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
fn entry_element(spec: &VariableSpec, kind: EntryKind, text: &str) -> Element {
    let cols_text;
    let rows_text;
    let mut attrs: Vec<(&str, &str)> = vec![("name", spec.name.as_str())];
    if let EntryShape::Array { cols, rows } = spec.shape {
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
        let msq = Msq::scaffold(&definition, &settings).expect("defaults scaffold");

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
        oxtuner_ini::parse_str_with_includes(text, &SettingsContext::new(), &|_: &str| None)
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
        assert!(saved.contains("<setting name=\"celsius\""), "{saved}");
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
