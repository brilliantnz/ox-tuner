//! The `.msq` document model over the raw-preserving XML tree.
//!
//! A `.msq` is thin XML: a `<versionInfo>` header, one leading **unnumbered**
//! `<page>` that holds the PC variables, then one `<page number="N">` per
//! calibration page (0-based), and a `<settings>` snapshot. Values are user
//! units keyed by name; there are no offsets in the file.
//!
//! Two rules make the file layer trustworthy:
//!
//! - **Pages are addressed by their `number` attribute, never by document
//!   order.** The leading PC page breaks any "the Nth `<page>` is page N"
//!   assumption (P10 finding 3).
//! - **The document is kept, not rebuilt.** This type exposes values and the
//!   one text mutation the save path needs; everything else — unknown
//!   attributes, comments, the `<bibliography>`, elements from a newer
//!   TunerStudio — serializes back byte-for-byte.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use oxtuner_ini::SettingsContext;
use oxtuner_tune::MsqPage;

use crate::encoding::{self, EncodeError};
use crate::xml::{Document, Element, Node, XmlError};

/// A `.msq` page address: the unnumbered PC page, or a numbered calibration page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntryPage {
    /// The leading page without a `number` attribute (PC variables).
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

/// Which kind of entry a value came from.
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

impl Msq {
    /// Parses `.msq` bytes (ISO-8859-1) into the document model.
    pub fn parse(bytes: &[u8]) -> Result<Self, MsqError> {
        let source = encoding::decode(bytes);
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
    pub fn save(&self) -> Result<Vec<u8>, MsqError> {
        Ok(encoding::encode(&self.document.serialize())?)
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
    /// page and is skipped here; [`Self::pc_pages`] reports those.
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

    /// Number of leading unnumbered pages (normally exactly one).
    pub fn pc_page_count(&self) -> usize {
        self.page_elements()
            .filter(|element| element.attr("number").is_none())
            .count()
    }

    /// Every value entry: the PC pages first, then numbered pages in file order.
    pub fn entries(&self) -> Vec<Entry> {
        let mut entries = Vec::new();
        for page in self.page_elements() {
            let address = match page.attr("number") {
                Some(number) => match number.trim().parse::<u16>() {
                    Ok(number) => EntryPage::Numbered(MsqPage::new(number)),
                    // A malformed number is not silently treated as page 0;
                    // the consistency check reports it as a blocker.
                    Err(_) => continue,
                },
                None => EntryPage::Pc,
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
    /// An entry whose name maps to a flag the definition tests is kept only
    /// when that flag is active; entries this build cannot map (TunerStudio
    /// setting-group choices) are preserved. Flags that became active but have
    /// no entry are appended, so a standalone reopen sees the same definition.
    ///
    /// A document that already agrees with the context is left untouched:
    /// TunerStudio writes a disabled boolean as an explicit `enablehardware_test_OFF`
    /// entry, and a save with no settings change must not delete it (the file
    /// has to stay byte-for-byte).
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
            let name = setting.attr("name").unwrap_or_default();
            let (flag, state) = flag_state(name);
            documented.insert(flag.to_ascii_lowercase(), state);
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
        for node in children {
            let keep = match &node {
                Node::Element(setting) if setting.name == "setting" => {
                    let name = setting.attr("name").unwrap_or_default();
                    let (flag, state) = flag_state(name);
                    let key = flag.to_ascii_lowercase();
                    if tested.contains(&key) {
                        let agrees = state == settings.is_active(&flag);
                        if agrees {
                            present.insert(key);
                        }
                        // An explicit `_OFF` entry agrees with an inactive flag
                        // and stays, so a one-flag change does not rewrite the
                        // rest of the snapshot.
                        agrees
                    } else {
                        true
                    }
                }
                _ => true,
            };
            if keep {
                kept.push(node);
            } else if matches!(kept.last(), Some(Node::Text(text)) if text.trim().is_empty()) {
                // Drop the indentation that belonged to the removed line, so a
                // removal does not leave a blank line behind.
                kept.pop();
            }
        }

        let missing: Vec<String> = settings
            .active_flags()
            .into_iter()
            .filter(|flag| tested.contains(flag) && !present.contains(flag))
            .collect();
        if !missing.is_empty() {
            // Reuse the closing indentation as the boundary for new entries.
            if matches!(kept.last(), Some(Node::Text(text)) if text.trim().is_empty()) {
                kept.pop();
            }
            for flag in missing {
                kept.push(Node::Text("\n        ".to_owned()));
                kept.push(Node::Element(Element::with_attrs(
                    "setting",
                    &[("name", &flag), ("value", &flag)],
                    true,
                )));
            }
            kept.push(Node::Text("\n    ".to_owned()));
        }
        element.children = kept;
        Ok(())
    }

    /// Replaces one entry's text, preserving everything else in the document.
    ///
    /// The caller supplies the final text (already formatted and *decoded*);
    /// XML escaping happens here.
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
            let address = match page_element.attr("number") {
                Some(number) => match number.trim().parse::<u16>() {
                    Ok(number) => EntryPage::Numbered(MsqPage::new(number)),
                    Err(_) => continue,
                },
                None => EntryPage::Pc,
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
                if entry.attr("name") == Some(name) {
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
    /// element carries only `name`; TunerStudio reads the rest from the INI.
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
                let address = match element.attr("number") {
                    Some(number) => number
                        .trim()
                        .parse::<u16>()
                        .ok()
                        .map(|number| EntryPage::Numbered(MsqPage::new(number))),
                    None => Some(EntryPage::Pc),
                };
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

        let mut element = Element::with_attrs(tag, &[("name", name)], false);
        element.set_text(text);
        page_element
            .children
            .push(Node::Text("\n        ".to_owned()));
        page_element.children.push(Node::Element(element));
        page_element.children.push(Node::Text("\n    ".to_owned()));
        Ok(())
    }
}

fn attribute_usize(element: &Element, name: &str) -> Option<usize> {
    element.attr(name)?.trim().parse().ok()
}

fn find_child_mut<'a>(element: &'a mut Element, name: &str) -> Option<&'a mut Element> {
    element.children.iter_mut().find_map(|node| match node {
        Node::Element(child) if child.name == name => Some(child),
        _ => None,
    })
}

/// Splits a setting name into the flag it selects and its state.
///
/// TunerStudio writes `enablehardware_test_OFF` for a disabled boolean and
/// `CELSIUS` for an enabled choice. The suffix convention is the only signal
/// the file carries, so it is made explicit here and covered by tests.
fn flag_state(name: &str) -> (String, bool) {
    if let Some(stem) = name
        .strip_suffix("_OFF")
        .or_else(|| name.strip_suffix("_off"))
    {
        return (stem.to_owned(), false);
    }
    if let Some(stem) = name
        .strip_suffix("_ON")
        .or_else(|| name.strip_suffix("_on"))
    {
        return (stem.to_owned(), true);
    }
    (name.to_owned(), true)
}

#[cfg(test)]
mod tests {
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
    fn latin1_bytes_survive_the_round_trip() {
        let source: &[u8] =
            b"<msq>\n  <bibliography author=\"Jos\xe9\"/>\n  <versionInfo signature=\"s\"/>\n</msq>";
        let msq = Msq::parse(source).expect("latin-1 parses");
        assert_eq!(msq.save().unwrap(), source);
    }

    #[test]
    fn a_setting_name_maps_to_its_flag() {
        assert_eq!(
            flag_state("enablehardware_test_OFF"),
            ("enablehardware_test".to_owned(), false)
        );
        assert_eq!(
            flag_state("enablehardware_test_ON"),
            ("enablehardware_test".to_owned(), true)
        );
        assert_eq!(flag_state("CELSIUS"), ("CELSIUS".to_owned(), true));
    }
}
