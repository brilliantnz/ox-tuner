//! The "is this tune usable?" gate (ADR-0011 S1).
//!
//! A `.msq` opens either fully or **read-only**. Structural disagreements —
//! a different firmware signature, a page count or size that does not match
//! the definition, a table whose declared shape contradicts the INI — are
//! blockers: converting values under those conditions would silently write
//! nonsense back to the ECU. Blockers therefore disable saving; there is no
//! "force" flag (ADR-0011).
//!
//! Everything else is a warning: a value the definition no longer declares
//! (the msq carries no offsets, so names are the only key), an INI constant
//! the file omits, a malformed optional attribute. Those values are loaded
//! where possible, preserved in the document, and reported once.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use oxtuner_ini::{Definition, Shape};
use oxtuner_tune::{EntryShape, IniPage, MsqPage, Tune, TuneLayout, VariableSpec};

use crate::msq::{EntryPage, Msq};

/// What a load check found.
#[expect(
    clippy::exhaustive_enums,
    reason = "load issues are a closed set of validation failures"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IssueKind {
    /// The file has no `<versionInfo>`.
    MissingVersionInfo,
    /// The firmware signature does not match the definition.
    SignatureMismatch,
    /// `nPages` does not match the definition's page count.
    PageCountMismatch,
    /// A `<page number>` is not valid.
    InvalidPageNumber,
    /// The same page number appears twice.
    DuplicatePage,
    /// The definition has a page the file does not.
    MissingPage,
    /// The file has a page the definition does not.
    UnknownPage,
    /// A page's `size` attribute contradicts the INI.
    PageSizeMismatch,
    /// A page has no `size` attribute to check.
    MissingPageSize,
    /// More or fewer than one PC page.
    PcPageCount,
    /// A table's `cols`/`rows` contradicts the INI shape.
    ShapeMismatch,
    /// A tune value has no matching definition entry.
    OrphanValue,
    /// A definition entry has no value in the file.
    MissingValue,
    /// An entry has no `name` attribute.
    NamelessEntry,
    /// The definition's presentation attributes could not be resolved.
    Definition,
    /// A value could not be loaded (bad text, wrong element count, …).
    ValueSkipped,
    /// A value was stored but lies outside the declared range.
    ValueOutOfRange,
    /// A `<pageData>` hex block disagrees with the decoded `<constant>` values.
    PageDataMismatch,
}

impl IssueKind {
    /// Whether the problem prevents saving.
    pub const fn is_blocker(self) -> bool {
        matches!(
            self,
            Self::MissingVersionInfo
                | Self::SignatureMismatch
                | Self::PageCountMismatch
                | Self::InvalidPageNumber
                | Self::DuplicatePage
                | Self::MissingPage
                | Self::UnknownPage
                | Self::PageSizeMismatch
                | Self::ShapeMismatch
        )
    }
}

/// One finding from the load gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadIssue {
    /// What kind of finding it is.
    pub kind: IssueKind,
    /// A message ready for the output panel.
    pub message: String,
}

impl fmt::Display for LoadIssue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

/// The result of checking a tune against its definition.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadReport {
    /// Problems that force read-only mode.
    pub blockers: Vec<LoadIssue>,
    /// Problems worth showing, but not fatal.
    pub warnings: Vec<LoadIssue>,
    /// Values copied into the tune.
    pub loaded: usize,
    /// Values left at their definition default.
    pub skipped: usize,
}

impl LoadReport {
    /// Whether the tune must open read-only.
    pub fn is_read_only(&self) -> bool {
        !self.blockers.is_empty()
    }

    /// The blocker messages, for the dialog that explains read-only mode.
    pub fn read_only_reasons(&self) -> Vec<&str> {
        self.blockers
            .iter()
            .map(|issue| issue.message.as_str())
            .collect()
    }

    /// A one-line summary for the status bar.
    pub fn summary(&self) -> String {
        use std::fmt::Write as _;

        let mut summary = if self.is_read_only() {
            format!("read-only ({} problems)", self.blockers.len())
        } else {
            format!("loaded {} values", self.loaded)
        };
        if !self.warnings.is_empty() {
            let _ = write!(summary, ", {} warnings", self.warnings.len());
        }
        summary
    }

    /// Records a finding in the appropriate list, according to its kind.
    pub fn push(&mut self, kind: IssueKind, message: impl Into<String>) {
        let issue = LoadIssue {
            kind,
            message: message.into(),
        };
        if kind.is_blocker() {
            self.blockers.push(issue);
        } else {
            self.warnings.push(issue);
        }
    }

    /// Records a warning.
    pub fn warn(&mut self, kind: IssueKind, message: impl Into<String>) {
        debug_assert!(
            !kind.is_blocker(),
            "a warning must not be recorded with a blocker kind"
        );
        self.warnings.push(LoadIssue {
            kind,
            message: message.into(),
        });
    }

    /// Records a blocker.
    pub fn block(&mut self, kind: IssueKind, message: impl Into<String>) {
        debug_assert!(
            kind.is_blocker(),
            "a blocker must be recorded with a blocker kind"
        );
        self.blockers.push(LoadIssue {
            kind,
            message: message.into(),
        });
    }
}

/// Checks a parsed `.msq` against the definition it claims to match.
pub fn check(msq: &Msq, definition: &Definition) -> LoadReport {
    let mut report = LoadReport::default();

    check_header(msq, definition, &mut report);
    check_pages(msq, definition, &mut report);
    check_entries(msq, definition, &mut report);
    report
}

fn check_header(msq: &Msq, definition: &Definition, report: &mut LoadReport) {
    let Some(info) = msq.version_info() else {
        report.block(
            IssueKind::MissingVersionInfo,
            "the tune has no <versionInfo>, so its firmware signature is unknown",
        );
        return;
    };

    if info.signature != definition.signature {
        let signature = if info.signature.is_empty() {
            "(none)"
        } else {
            info.signature.as_str()
        };
        report.block(
            IssueKind::SignatureMismatch,
            format!(
                "the tune is for firmware `{signature}`, the definition is for `{}`",
                definition.signature
            ),
        );
    }

    let expected = u32::try_from(definition.pages.len()).unwrap_or(u32::MAX);
    match info.n_pages {
        Some(pages) if pages == expected => {}
        Some(pages) => report.block(
            IssueKind::PageCountMismatch,
            format!("the tune declares {pages} pages, the definition has {expected}"),
        ),
        None => report.block(
            IssueKind::PageCountMismatch,
            "the tune has no valid `nPages` attribute",
        ),
    }
}

fn check_pages(msq: &Msq, definition: &Definition, report: &mut LoadReport) {
    let mut from_tune: BTreeMap<IniPage, u32> = BTreeMap::new();

    if let Some(root) = msq.document().root() {
        for page in root.find_children("page") {
            let Some(number) = page.attr("number") else {
                continue;
            };
            let Ok(number) = number.trim().parse::<i64>() else {
                report.block(
                    IssueKind::InvalidPageNumber,
                    format!("`number=\"{number}\"` is not a valid page number"),
                );
                continue;
            };
            if number < 0 {
                // `-1` is the page that holds PC variables, not calibration
                // data, so it has no INI counterpart to compare against.
                continue;
            }
            let Ok(number) = u16::try_from(number) else {
                report.block(
                    IssueKind::InvalidPageNumber,
                    format!("`number=\"{number}\"` is not a valid page number"),
                );
                continue;
            };
            let Some(ini) = oxtuner_tune::MsqPage::new(number).to_ini() else {
                report.block(
                    IssueKind::UnknownPage,
                    format!("page {number} has no 1-based INI counterpart"),
                );
                continue;
            };

            let size = page
                .attr("size")
                .and_then(|size| size.trim().parse::<u32>().ok());
            if from_tune.insert(ini, size.unwrap_or(u32::MAX)).is_some() {
                report.block(
                    IssueKind::DuplicatePage,
                    format!("page {number} appears more than once"),
                );
                continue;
            }

            match (
                definition
                    .pages
                    .iter()
                    .find(|entry| entry.number == ini.get()),
                size,
            ) {
                (None, _) => report.block(
                    IssueKind::UnknownPage,
                    format!(
                        "the tune has page {number}, the definition has no page {}",
                        ini.get()
                    ),
                ),
                (Some(_), None) => report.warn(
                    IssueKind::MissingPageSize,
                    format!("page {number} has no `size` attribute to verify"),
                ),
                (Some(entry), Some(size)) if size != entry.size => report.block(
                    IssueKind::PageSizeMismatch,
                    format!(
                        "page {number} is {size} bytes in the tune, {} in the definition",
                        entry.size
                    ),
                ),
                (Some(_), Some(_)) => {}
            }
        }
    }

    for page in &definition.pages {
        let Some(ini) = IniPage::new(page.number) else {
            continue;
        };
        if !from_tune.contains_key(&ini) {
            report.block(
                IssueKind::MissingPage,
                format!(
                    "the definition has page {} ({} bytes), the tune does not",
                    ini.get(),
                    page.size
                ),
            );
        }
    }

    let pc_pages = msq.pc_page_count();
    if pc_pages != 1 {
        report.warn(
            IssueKind::PcPageCount,
            format!("expected one PC variable page, found {pc_pages}"),
        );
    }
}

fn check_entries(msq: &Msq, definition: &Definition, report: &mut LoadReport) {
    let entries = msq.entries();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut orphans: Vec<&str> = Vec::new();
    let mut nameless = 0usize;

    for entry in &entries {
        if entry.is_nameless() {
            nameless += 1;
            continue;
        }
        seen.insert(entry.name.as_str());

        let constant = definition.constant(&entry.name);
        let pc_variable = definition.pc_variables.get(&entry.name);
        if constant.is_none() && pc_variable.is_none() {
            orphans.push(entry.name.as_str());
            continue;
        }

        if let Some(constant) = constant {
            let tune_cols = entry.cols.and_then(|value| u32::try_from(value).ok());
            let tune_rows = entry.rows.and_then(|value| u32::try_from(value).ok());
            match &constant.shape {
                Shape::Table { cols, rows } => {
                    let declared_cols = cols.value();
                    let declared_rows = rows.value();
                    if tune_cols.is_some() && tune_cols != declared_cols
                        || tune_rows.is_some() && tune_rows != declared_rows
                    {
                        report.block(
                            IssueKind::ShapeMismatch,
                            format!(
                                "`{}` is {}×{} in the tune but {}×{} in the definition",
                                entry.name,
                                tune_cols.map_or_else(|| "?".to_owned(), |v| v.to_string()),
                                tune_rows.map_or_else(|| "?".to_owned(), |v| v.to_string()),
                                declared_cols.map_or_else(|| "?".to_owned(), |v| v.to_string()),
                                declared_rows.map_or_else(|| "?".to_owned(), |v| v.to_string()),
                            ),
                        );
                    }
                }
                Shape::List(length) => {
                    // A list has one dimension, but TunerStudio writes it as
                    // `cols="1" rows="N"` while other tools may write `N×1`.
                    // Compare the element count, not the orientation.
                    let declared = length.value();
                    let found = match (tune_cols, tune_rows) {
                        (Some(cols), Some(rows)) => Some(cols.saturating_mul(rows)),
                        (Some(cols), None) => Some(cols),
                        (None, Some(rows)) => Some(rows),
                        (None, None) => None,
                    };
                    if let (Some(found), Some(declared)) = (found, declared)
                        && found != declared
                    {
                        report.block(
                            IssueKind::ShapeMismatch,
                            format!(
                                "`{}` has {found} values in the tune but {declared} in the definition",
                                entry.name
                            ),
                        );
                    }
                }
                Shape::Scalar | Shape::Bits { .. } | Shape::Text { .. } => {}
            }
        }
    }

    if nameless > 0 {
        report.warn(
            IssueKind::NamelessEntry,
            format!("{nameless} entries have no `name` attribute and were ignored"),
        );
    }
    if !orphans.is_empty() {
        report.warn(
            IssueKind::OrphanValue,
            format!(
                "{} values have no definition entry and were preserved untouched{}",
                orphans.len(),
                preview(&orphans)
            ),
        );
    }

    let mut missing: Vec<&str> = definition
        .constants
        .iter()
        .filter(|(_, constant)| !constant.flags.no_msq_save)
        .map(|(name, _)| name)
        .chain(definition.pc_variables.iter().map(|(name, _)| name))
        .filter(|name| !seen.contains(*name))
        .collect();
    missing.sort_unstable();
    if !missing.is_empty() {
        report.warn(
            IssueKind::MissingValue,
            format!(
                "{} definition entries have no value in the tune; their defaults are used{}",
                missing.len(),
                preview(&missing)
            ),
        );
    }
}

/// Cross-checks a `<pageData>` hex block against the values the tune decoded.
///
/// Some writers store both spellings of the same page. The decoded constants
/// are authoritative, so a disagreement is a warning: it means the file was
/// written by a tool whose own view of the page drifted, and saying so is more
/// useful than silently picking one. A hidden option (`INVALID`) is skipped,
/// because any byte means "invalid" and there is nothing to compare.
pub(crate) fn check_page_data(
    msq: &Msq,
    definition: &Definition,
    layout: &TuneLayout,
    tune: &Tune,
    report: &mut LoadReport,
) {
    let blocks = msq.page_data();
    if blocks.is_empty() {
        return;
    }

    let mut entries_by_page: BTreeMap<u16, Vec<String>> = BTreeMap::new();
    for entry in msq.entries() {
        if let EntryPage::Numbered(page) = entry.page
            && !entry.is_nameless()
        {
            entries_by_page
                .entry(page.get())
                .or_default()
                .push(entry.name.clone());
        }
    }

    let layout_arc = std::sync::Arc::new(layout.clone());

    for (number, bytes) in blocks {
        let Ok(number) = u16::try_from(number) else {
            continue;
        };
        let page_number = MsqPage::new(number);
        let Some(ini) = page_number.to_ini() else {
            continue;
        };
        let Some(page_layout) = layout.page(ini) else {
            continue;
        };

        let expected = usize::try_from(page_layout.size).unwrap_or(0);
        if bytes.len() != expected {
            report.warn(
                IssueKind::PageDataMismatch,
                format!(
                    "page {number} carries {} page-data bytes, the definition says {expected}",
                    bytes.len()
                ),
            );
            continue;
        }

        let mut scratch = Tune::new(std::sync::Arc::clone(&layout_arc));
        if oxtuner_tune::decode_page(layout, ini, &bytes, &mut scratch).is_err() {
            continue;
        }

        let mut mismatched: Vec<&str> = Vec::new();
        for name in entries_by_page
            .get(&number)
            .into_iter()
            .flatten()
            .map(String::as_str)
        {
            if let Some(spec) = layout.variable(name)
                && selects_hidden_option(definition, spec, tune)
            {
                continue;
            }

            if tune.value(name) != scratch.value(name) {
                mismatched.push(name);
            }
        }

        if !mismatched.is_empty() {
            let count = mismatched.len();
            let noun = if count == 1 { "value" } else { "values" };

            report.warn(
                IssueKind::PageDataMismatch,
                format!(
                    "page {number} page data disagrees with {count} {noun}{}",
                    preview(&mismatched)
                ),
            );
        }
    }
}

/// Whether the loaded value selects an option the definition marks hidden.
///
/// `INVALID` has no stable byte value: a writer may store its list position or
/// a sentinel such as `0xFF`. There is nothing meaningful to compare.
#[expect(
    clippy::float_cmp,
    reason = "a bit option matches its exact integer raw value"
)]
fn selects_hidden_option(definition: &Definition, spec: &VariableSpec, tune: &Tune) -> bool {
    if !matches!(spec.shape, EntryShape::Bits { .. }) {
        return false;
    }

    let Some(constant) = definition.constant(&spec.name) else {
        return false;
    };
    let Ok(raw) = tune.scalar_raw(&spec.name) else {
        return false;
    };

    constant
        .options
        .iter()
        .any(|option| option.hidden && f64::from(option.value) == raw)
}

fn preview(names: &[&str]) -> String {
    const LIMIT: usize = 5;
    let mut text = String::new();
    for name in names.iter().take(LIMIT) {
        text.push_str(if text.is_empty() { " (" } else { ", " });
        text.push_str(name);
    }
    if !text.is_empty() {
        if names.len() > LIMIT {
            text.push_str(", …");
        }
        text.push(')');
    }
    text
}

#[cfg(test)]
mod tests {
    use oxtuner_ini::{SettingsContext, parse_str};

    use super::*;
    use crate::msq::Msq;

    const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"

[Constants]
nPages = 2
pageSize = 128, 288
page = 1
a = scalar, U08, 0, "", 1, 0, 0, 255, 0
table = array, U08, 1, [2x2], "", 1, 0, 0, 255, 0
list = array, U08, 5, [4], "", 1, 0, 0, 255, 0
page = 2
b = scalar, U08, 0, "", 1, 0, 0, 255, 0

[PcVariables]
pc = scalar, U08, 0, "", 1, 0, 0, 255, 0
"#;

    const MSQ: &str = r#"<msq>
    <versionInfo fileFormat="5.0" nPages="2" signature="speeduino 202501"/>
    <page>
        <pcVariable name="pc">1.0</pcVariable>
    </page>
    <page number="0" size="128">
        <constant name="a">1.0</constant>
        <constant cols="2" rows="2" name="table">1.0 2.0 3.0 4.0</constant>
        <constant cols="1" rows="4" name="list">1.0 2.0 3.0 4.0</constant>
    </page>
    <page number="1" size="288">
        <constant name="b">2.0</constant>
    </page>
    <settings><setting name="CELSIUS" value="CELSIUS"/></settings>
</msq>"#;

    fn definition() -> Definition {
        parse_str(DEFINITION, &SettingsContext::new()).expect("definition parses")
    }

    fn msq(source: &str) -> Msq {
        Msq::parse(source.as_bytes()).expect("tune parses")
    }

    #[test]
    fn a_matching_tune_has_no_blockers() {
        let report = check(&msq(MSQ), &definition());
        assert!(!report.is_read_only(), "{:?}", report.blockers);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert!(report.summary().starts_with("loaded"));
    }

    #[test]
    fn a_signature_mismatch_is_a_blocker() {
        let source = MSQ.replace("speeduino 202501", "speeduino 202412");
        let report = check(&msq(&source), &definition());
        assert!(report.is_read_only());
        assert!(report.read_only_reasons()[0].contains("202412"));
    }

    #[test]
    fn page_sizes_and_counts_are_blockers() {
        let size = MSQ.replace("size=\"288\"", "size=\"256\"");
        assert!(check(&msq(&size), &definition()).is_read_only());

        let count = MSQ.replace("nPages=\"2\"", "nPages=\"3\"");
        assert!(check(&msq(&count), &definition()).is_read_only());

        let missing = MSQ.replace("nPages=\"2\"", "");
        assert!(check(&msq(&missing), &definition()).is_read_only());
    }

    #[test]
    fn a_missing_or_extra_page_is_a_blocker() {
        let missing = MSQ.replace(
            "<page number=\"1\" size=\"288\">\n        <constant name=\"b\">2.0</constant>\n    </page>",
            "",
        );
        assert!(check(&msq(&missing), &definition()).is_read_only());
    }

    #[test]
    fn an_orphan_name_is_reported_once_as_a_warning() {
        let source = MSQ.replace(
            "<constant name=\"b\">2.0</constant>",
            "<constant name=\"b\">2.0</constant><constant name=\"gone\">0.0</constant>",
        );
        let report = check(&msq(&source), &definition());
        assert!(!report.is_read_only());
        let orphan = report
            .warnings
            .iter()
            .find(|issue| issue.kind == IssueKind::OrphanValue)
            .expect("orphan warning");
        assert!(orphan.message.contains("gone"));
    }

    #[test]
    fn a_table_shape_mismatch_is_a_blocker() {
        let source = MSQ.replace("cols=\"2\" rows=\"2\"", "cols=\"4\" rows=\"1\"");
        let report = check(&msq(&source), &definition());
        assert!(report.is_read_only());
        assert!(
            report
                .blockers
                .iter()
                .any(|issue| issue.kind == IssueKind::ShapeMismatch)
        );
    }

    #[test]
    fn a_list_is_checked_by_element_count_not_orientation() {
        let report = check(&msq(MSQ), &definition());
        assert!(!report.is_read_only(), "{:?}", report.blockers);

        let transposed = MSQ.replace(
            "cols=\"1\" rows=\"4\" name=\"list\"",
            "cols=\"4\" rows=\"1\" name=\"list\"",
        );
        assert!(!check(&msq(&transposed), &definition()).is_read_only());

        let short = MSQ.replace(
            "cols=\"1\" rows=\"4\" name=\"list\"",
            "cols=\"1\" rows=\"5\" name=\"list\"",
        );
        let report = check(&msq(&short), &definition());
        assert!(
            report
                .blockers
                .iter()
                .any(|issue| issue.kind == IssueKind::ShapeMismatch
                    && issue.message.contains("list"))
        );
    }

    #[test]
    fn a_missing_value_is_a_warning_and_uses_the_default() {
        let source = MSQ.replace("<constant name=\"a\">1.0</constant>", "");
        let report = check(&msq(&source), &definition());
        assert!(!report.is_read_only());
        assert!(
            report
                .warnings
                .iter()
                .any(|issue| issue.kind == IssueKind::MissingValue)
        );
    }

    #[test]
    fn a_negative_pc_page_is_not_reported() {
        // A negative number is the same page as an unnumbered one and must not
        // raise `PcPageCount`.
        let source = MSQ.replace("    <page>\n        <pcVariable name=\"pc\">1.0</pcVariable>\n    </page>",
            "    <page number=\"-1\">\n        <pcVariable name=\"pc\">1.0</pcVariable>\n    </page>");
        let report = check(&msq(&source), &definition());

        assert!(!report.is_read_only(), "{:?}", report.blockers);
        assert!(
            !report
                .warnings
                .iter()
                .any(|issue| issue.kind == IssueKind::PcPageCount),
            "{:?}",
            report.warnings
        );
    }
}
