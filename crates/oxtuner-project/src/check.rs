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
    /// A value is outside its declared range or does not fit its storage type.
    ///
    /// The value is kept: a controller can hold a byte its definition's range
    /// does not cover, and a file states what it states. Only a value the
    /// storage type cannot hold is clamped, and that is reported here too.
    ValueOutOfRange,
    /// A `<pageData>` hex block disagrees with the decoded `<constant>` values.
    PageDataMismatch,
}

impl IssueKind {
    /// A stable code for logs and a future notification policy.
    ///
    /// Never contains user data, so it is safe to group or filter on.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::MissingVersionInfo => "missing-version-info",
            Self::SignatureMismatch => "signature-mismatch",
            Self::PageCountMismatch => "page-count-mismatch",
            Self::InvalidPageNumber => "invalid-page-number",
            Self::DuplicatePage => "duplicate-page",
            Self::MissingPage => "missing-page",
            Self::UnknownPage => "unknown-page",
            Self::PageSizeMismatch => "page-size-mismatch",
            Self::MissingPageSize => "missing-page-size",
            Self::PcPageCount => "pc-page-count",
            Self::ShapeMismatch => "shape-mismatch",
            Self::OrphanValue => "orphan-value",
            Self::MissingValue => "missing-value",
            Self::NamelessEntry => "nameless-entry",
            Self::Definition => "definition",
            Self::ValueSkipped => "value-skipped",
            Self::ValueOutOfRange => "value-out-of-range",
            Self::PageDataMismatch => "page-data-mismatch",
        }
    }

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
///
/// Non-exhaustive: a new finding list is additive, and every field has a
/// default, so a caller reads the fields it needs and ignores the rest.
#[non_exhaustive]
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
    /// Names of values the definition does not declare, in file order.
    ///
    /// These are the part of [`Self::skipped`] that carries no
    /// [`IssueKind::ValueSkipped`] warning of its own: the check layer reports
    /// them once, with a short preview. The full list is kept here so a log or
    /// a diagnostics view can name every one instead of only counting them.
    pub orphan_values: Vec<String>,
    /// Names of definition entries the tune has no value for, sorted.
    ///
    /// The check layer warns about them once, with a short preview; this is the
    /// full list, for the same reason as [`Self::orphan_values`].
    pub missing_values: Vec<String>,
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
    let mut from_tune: BTreeSet<IniPage> = BTreeSet::new();

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
            if !from_tune.insert(ini) {
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
        if !from_tune.contains(&ini) {
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

/// The definition entries the tune has no value for, sorted.
///
/// Constants and PC variables marked `noMsqSave` are never written, so their
/// absence is not a missing value. Names are matched case-insensitively, the
/// way a lookup by name works.
fn missing_entries<'a>(definition: &'a Definition, seen: &BTreeSet<String>) -> Vec<&'a str> {
    let mut missing: Vec<&str> = definition
        .constants
        .iter()
        .filter(|(_, constant)| !constant.flags.no_msq_save)
        .map(|(name, _)| name)
        .chain(
            definition
                .pc_variables
                .iter()
                .filter(|(_, variable)| !variable.flags.no_msq_save)
                .map(|(name, _)| name),
        )
        .filter(|name| !seen.contains(&name.to_ascii_lowercase()))
        .collect();

    missing.sort_unstable();
    missing
}

/// Drops derived views from the missing-value report.
///
/// A view is removed from a saved file on purpose; a reopen derives it from
/// its owner, so reporting it as missing would be false.
pub(crate) fn drop_derived_missing(report: &mut LoadReport, is_derived: impl Fn(&str) -> bool) {
    let before = report.missing_values.len();
    report.missing_values.retain(|name| !is_derived(name));
    if report.missing_values.len() == before {
        return;
    }

    report
        .warnings
        .retain(|issue| issue.kind != IssueKind::MissingValue);
    if !report.missing_values.is_empty() {
        let missing: Vec<&str> = report.missing_values.iter().map(String::as_str).collect();
        report.warn(
            IssueKind::MissingValue,
            format!(
                "{} definition entries have no value in the tune; their defaults are used{}",
                missing.len(),
                preview(&missing),
            ),
        );
    }
}

fn check_entries(msq: &Msq, definition: &Definition, report: &mut LoadReport) {
    let entries = msq.entries();
    // Case-folded, because a lookup by name is case-insensitive: an entry that
    // loaded under another case must not be reported missing as well.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut orphans: Vec<&str> = Vec::new();
    let mut nameless = 0usize;

    for entry in &entries {
        if entry.is_nameless() {
            nameless += 1;
            continue;
        }
        seen.insert(entry.name.to_ascii_lowercase());

        let constant = definition.constant(&entry.name);
        let pc_variable = definition.pc_variables.get_ignore_case(&entry.name);
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
        report.orphan_values = orphans.iter().map(|name| (*name).to_owned()).collect();
        report.warn(
            IssueKind::OrphanValue,
            format!(
                "{} values have no definition entry and were preserved untouched{}",
                orphans.len(),
                preview(&orphans)
            ),
        );
    }

    let missing = missing_entries(definition, &seen);
    if !missing.is_empty() {
        report.missing_values = missing.iter().map(|name| (*name).to_owned()).collect();
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
    layout: &std::sync::Arc<TuneLayout>,
    tune: &Tune,
    report: &mut LoadReport,
) {
    let blocks = msq.page_data();
    if blocks.is_empty() {
        return;
    }

    // Layout indices rather than names: the comparison below runs once per
    // entry, and every name lookup is a hash over the whole name.
    let mut entries_by_page: BTreeMap<u16, Vec<usize>> = BTreeMap::new();
    for entry in msq.entries() {
        if let EntryPage::Numbered(page) = entry.page
            && !entry.is_nameless()
            && let Some(variable) = layout.index(&entry.name)
        {
            entries_by_page
                .entry(page.get())
                .or_default()
                .push(variable);
        }
    }

    // One scratch image for every page. A constant belongs to exactly one page,
    // so the names compared on one page are never written by another, and
    // rebuilding the image per page would allocate once per variable per page.
    let mut scratch = Tune::new(std::sync::Arc::clone(layout));

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

        // A page with no single owner for its shared bytes keeps the file's
        // values, so its block cannot be compared value by value. A wrong byte
        // count is still a defect and was reported above.
        if layout.page_has_ambiguous_overlap(ini) {
            continue;
        }

        if oxtuner_tune::decode_page(layout, ini, &bytes, &mut scratch).is_err() {
            continue;
        }

        let mut mismatched: Vec<&str> = Vec::new();
        for &variable in entries_by_page.get(&number).into_iter().flatten() {
            let spec = &layout.variables()[variable];
            if selects_hidden_option(definition, spec, tune) {
                continue;
            }

            if tune.values().get(variable) != scratch.values().get(variable) {
                mismatched.push(&spec.name);
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
    fn issue_kinds_carry_stable_codes_for_logs() {
        assert_eq!(
            IssueKind::SignatureMismatch.code(),
            "signature-mismatch",
            "a blocker keeps its code when the message changes"
        );
        assert_eq!(IssueKind::OrphanValue.code(), "orphan-value");
        assert_eq!(IssueKind::ValueSkipped.code(), "value-skipped");
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
