//! Pass 2–4: sections → resolved model → frozen [`Definition`].
//!
//! Sections are read **in document order**, because the dialect is order
//! sensitive: a constant referenced in another constant's expression must be
//! declared earlier, and `[PcVariables]` load before `[Constants]` on purpose
//! (reference §Section load order).
//!
//! Nothing here panics. A section this build does not model is kept verbatim in
//! [`Definition::unsupported`]; a key inside a known section that we do not
//! understand is reported once and left out of the model.

mod channels;
mod constants;
mod dashboard;
mod dialogs;
mod editors;
mod misc;

use std::collections::BTreeSet;

use crate::error::{Diagnostic, IniError, Severity};
use crate::model::{Coverage, Definition, SectionCoverage};
use crate::preprocess::{Expander, IncludeResolver, LogicalLine};
use crate::settings::SettingsContext;
use crate::source;

/// The highest `iniSpecVersion` this build can read.
///
/// A file that declares a newer spec must fail loudly rather than be
/// half-parsed, the way the reference application refuses it.
pub const MAX_SPEC_VERSION: f64 = 3.80;

/// Version of the parser's output contract.
///
/// Bump this whenever the same input bytes and settings can produce a
/// different `Definition` (a grammar fix, a new section, a changed default).
/// The definition cache includes it in its key, so an upgrade misses instead
/// of serving a definition this build no longer agrees with.
pub const PARSER_VERSION: u32 = 1;

/// Parses bytes into a frozen definition.
///
/// # Errors
///
/// Returns [`IniError`] when the definition has a structural problem, such as a
/// missing signature or an unbalanced conditional block.
pub fn parse_bytes(bytes: &[u8], settings: &SettingsContext) -> Result<Definition, IniError> {
    let decoded = source::decode(bytes);
    assemble(
        &decoded.text,
        settings,
        None,
        decoded.diagnostic.into_iter().collect(),
    )
}

/// Parses text into a frozen definition.
///
/// # Errors
///
/// Returns [`IniError`] when the definition has a structural problem, such as a
/// missing signature or an unbalanced conditional block.
pub fn parse_str(text: &str, settings: &SettingsContext) -> Result<Definition, IniError> {
    assemble(text, settings, None, Vec::new())
}

/// Parses text, inlining `#include` files through `resolver`.
///
/// # Errors
///
/// Returns [`IniError`] when the definition has a structural problem, or when
/// an included file cannot be preprocessed.
pub fn parse_str_with_includes(
    text: &str,
    settings: &SettingsContext,
    resolver: IncludeResolver<'_>,
) -> Result<Definition, IniError> {
    assemble(text, settings, Some(resolver), Vec::new())
}

fn assemble(
    text: &str,
    settings: &SettingsContext,
    includes: Option<IncludeResolver<'_>>,
    extra_diagnostics: Vec<Diagnostic>,
) -> Result<Definition, IniError> {
    let preprocessed = {
        let _span = tracing::debug_span!("ini_preprocess").entered();
        crate::preprocess::preprocess(text, settings, includes)?
    };

    let mut definition = Definition::empty();
    definition.defines = preprocessed.defines.clone();
    definition
        .tested_flags
        .clone_from(preprocessed.settings.tested_flags());
    definition.active_flags = preprocessed.settings.active_flags();
    definition.diagnostics.extend(extra_diagnostics);
    definition
        .diagnostics
        .extend(preprocessed.diagnostics.iter().cloned());

    let expander = Expander::new(&preprocessed.defines);

    {
        let _span = tracing::debug_span!("ini_parse_sections").entered();
        let sections = split_sections(&preprocessed.lines);

        let mut coverage = Coverage::default();
        for section in &sections {
            let mut parser = SectionParser::new(&section.name);
            dispatch(&mut definition, &mut parser, &section.lines, &expander);
            parser.finish(&mut definition, &mut coverage);
        }

        definition.coverage = coverage;
    }

    let _span = tracing::debug_span!("ini_resolve_validate").entered();

    if definition.signature.is_empty() {
        return Err(IniError::new(vec![Diagnostic::whole_file(
            Severity::Error,
            "missing-signature",
            "the definition has no `signature` in `[MegaTune]` or `[TunerStudio]`",
        )]));
    }

    if let Some(version) = definition.spec_version
        && version > MAX_SPEC_VERSION
    {
        return Err(IniError::new(vec![Diagnostic::whole_file(
            Severity::Error,
            "unsupported-spec",
            format!(
                "the definition declares iniSpecVersion {version}; this build supports up to \
                 {MAX_SPEC_VERSION} — update ox-tuner instead of reading it partially"
            ),
        )]));
    }

    finalize(&mut definition);
    Ok(definition)
}

/// One `[section]` and the lines that belong to it. Text before the first
/// header belongs to the empty-named preamble (rusEFI has one).
struct RawSection<'a> {
    name: String,
    lines: Vec<(u32, &'a str)>,
}

fn split_sections(lines: &[LogicalLine]) -> Vec<RawSection<'_>> {
    let mut sections: Vec<RawSection<'_>> = vec![RawSection {
        name: String::new(),
        lines: Vec::new(),
    }];

    for line in lines {
        if let Some(name) = section_header(&line.text) {
            sections.push(RawSection {
                name: name.to_owned(),
                lines: Vec::new(),
            });
            continue;
        }
        if let Some(current) = sections.last_mut() {
            current.lines.push((line.line, line.text.as_str()));
        }
    }

    sections
}

fn section_header(text: &str) -> Option<&str> {
    let text = text.trim();
    let inner = text.strip_prefix('[')?.strip_suffix(']')?;
    Some(inner.trim())
}

fn dispatch(
    definition: &mut Definition,
    section: &mut SectionParser,
    lines: &[(u32, &str)],
    expander: &Expander<'_>,
) {
    match section.name.to_ascii_lowercase().as_str() {
        "megatune" | "tunerstudio" => constants::parse_identification(definition, section, lines),
        "settinggroups" => dialogs::parse_setting_groups(definition, section, lines),
        "pcvariables" => constants::parse_pc_variables(definition, section, lines, expander),
        "constants" => constants::parse_constants(definition, section, lines, expander),
        "constantsextensions" | "constantextensions" | "defaults" => {
            misc::parse_extensions(definition, section, lines);
        }
        "outputchannels" => channels::parse_channels(definition, section, lines, expander),
        "burstmode" => channels::parse_burst_mode(definition, section, lines),
        "tableeditor" => editors::parse_tables(definition, section, lines),
        "curveeditor" => editors::parse_curves(definition, section, lines),
        "gaugeconfigurations" => dashboard::parse_gauges(definition, section, lines),
        "frontpage" => dashboard::parse_front_page(definition, section, lines),
        "userdefined" | "uidialogs" => dialogs::parse_dialogs(definition, section, lines),
        "menu" => dialogs::parse_menu(definition, section, lines),
        "settingcontexthelp" => dialogs::parse_context_help(definition, section, lines),
        "keyactions" => dialogs::parse_key_actions(definition, section, lines),
        "controllercommands" => misc::parse_controller_commands(definition, section, lines),
        "referencetables" => misc::parse_reference_tables(definition, section, lines),
        "datalog" => misc::parse_datalog(definition, section, lines),
        "" => {
            // File-level attributes before the first header. rusEFI opens with
            // `enable2ndByteCanID = false`; keep it, do not reject the file.
            for (line, text) in lines {
                section.count_entry();
                if let Some((key, value)) = crate::fields::split_assignment(text) {
                    definition
                        .comms
                        .raw
                        .insert(key.trim().to_owned(), value.trim().to_owned());
                    section.understand(*line);
                } else {
                    section.unrecognized_line(*line, text);
                }
            }
        }
        _ => {
            // Unknown section: data, never an error (ADR-0006 decision 4).
            let mut raw = Vec::new();
            for (line, text) in lines {
                section.total = section.total.saturating_add(1);
                raw.push((*text).to_owned());
                let _ = line;
            }
            definition
                .unsupported
                .push(crate::model::UnsupportedSection {
                    name: section.name.clone(),
                    lines: raw,
                });
            section.kept_raw = true;
        }
    }
}

/// Counts what one section understood.
pub(crate) struct SectionParser {
    /// Section name as written.
    pub(crate) name: String,
    /// Lines turned into model data.
    understood: BTreeSet<u32>,
    /// Lines seen.
    pub(crate) total: usize,
    /// Keys reported as unrecognized, so each is logged once.
    reported_keys: BTreeSet<String>,
    /// Diagnostics for this section.
    pub(crate) diagnostics: Vec<Diagnostic>,
    /// Whether the whole section was kept as raw data.
    pub(crate) kept_raw: bool,
}

impl SectionParser {
    pub(crate) fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            understood: BTreeSet::new(),
            total: 0,
            reported_keys: BTreeSet::new(),
            diagnostics: Vec::new(),
            kept_raw: false,
        }
    }

    /// Records that another entry was seen.
    pub(crate) fn count_entry(&mut self) {
        self.total = self.total.saturating_add(1);
    }

    /// Records that this line became model data.
    pub(crate) fn understand(&mut self, line: u32) {
        self.understood.insert(line);
    }

    /// Records a warning tied to a line.
    pub(crate) fn warn(&mut self, code: &'static str, message: impl Into<String>, line: u32) {
        self.diagnostics.push(Diagnostic::new(
            Severity::Warning,
            code,
            message,
            line,
            &self.name,
        ));
    }

    /// Records an informational note tied to a line.
    pub(crate) fn note(&mut self, code: &'static str, message: impl Into<String>, line: u32) {
        self.diagnostics.push(Diagnostic::new(
            Severity::Note,
            code,
            message,
            line,
            &self.name,
        ));
    }

    /// Records an error tied to a line.
    pub(crate) fn error(&mut self, code: &'static str, message: impl Into<String>, line: u32) {
        self.diagnostics.push(Diagnostic::new(
            Severity::Error,
            code,
            message,
            line,
            &self.name,
        ));
    }

    /// A line inside a known section that is not an assignment and has no
    /// meaning here.
    pub(crate) fn unrecognized_line(&mut self, line: u32, text: &str) {
        self.note(
            "unrecognized-line",
            format!("unrecognized line `{}`", text.trim()),
            line,
        );
    }

    /// A key this build does not model. Reported once per key per file.
    pub(crate) fn unrecognized_key(&mut self, key: &str, line: u32) {
        let lowered = key.to_ascii_lowercase();
        if self.reported_keys.insert(lowered) {
            self.note(
                "unrecognized-key",
                format!("`{key}` is not modelled yet; the entry is kept out of the model"),
                line,
            );
        }
    }

    fn finish(self, definition: &mut Definition, coverage: &mut Coverage) {
        let understood = self.understood.len();
        let entries = self.total;
        let unsupported = entries.saturating_sub(understood);
        if !self.name.is_empty() || entries > 0 {
            coverage.sections.push(SectionCoverage {
                name: if self.name.is_empty() {
                    "preamble".to_owned()
                } else {
                    self.name.clone()
                },
                entries,
                understood,
                unsupported,
            });
        }
        definition.diagnostics.extend(self.diagnostics);
    }
}

/// Convenience used by every section parser: count the entry, then decide.
pub(crate) fn entry_line(
    section: &mut SectionParser,
    line: u32,
    text: &str,
) -> Option<(String, String)> {
    section.count_entry();
    if let Some((key, value)) = crate::fields::split_assignment(text) {
        Some((key.trim().to_owned(), value.trim().to_owned()))
    } else {
        section.unrecognized_line(line, text);
        None
    }
}

/// Cross-section validation and reference resolution.
fn finalize(definition: &mut Definition) {
    apply_extension_flags(definition);
    validate_pages(definition);
    validate_references(definition);
}

fn apply_extension_flags(definition: &mut Definition) {
    // The lists live beside the constants, not inside them: destructuring
    // borrows each field once, so the sets are read in place instead of being
    // cloned (`readOnly` can name every constant in the file).
    let Definition {
        extensions,
        constants,
        ..
    } = definition;

    for (name, constant) in constants.iter_mut() {
        if extensions.read_only.contains(name) {
            constant.flags.controller_priority |= true;
        }
        if extensions.controller_priority.contains(name) {
            constant.flags.controller_priority = true;
        }
    }
}

fn validate_pages(definition: &mut Definition) {
    let mut problems = Vec::new();

    for (name, constant) in definition.constants.iter() {
        let page_number = constant.page;
        if page_number == 0 {
            continue;
        }
        let Some(page) = definition.page(page_number) else {
            problems.push(Diagnostic::new(
                Severity::Warning,
                "unknown-page",
                format!(
                    "`{name}` is declared on page {page_number}, which the file does not define"
                ),
                constant.line,
                "Constants",
            ));
            continue;
        };

        if let (Some(address), Some(size)) = (constant.address, constant.byte_size())
            && size > 0
            && address.saturating_add(size) > page.size
        {
            problems.push(Diagnostic::new(
                Severity::Error,
                "offset-outside-page",
                format!(
                    "`{name}` needs bytes {address}..{} on page {page_number} of {} bytes",
                    address.saturating_add(size),
                    page.size
                ),
                constant.line,
                "Constants",
            ));
        }

        if let Some(scale) = &constant.scale
            && scale.literal() == Some(0.0)
        {
            problems.push(Diagnostic::new(
                Severity::Warning,
                "zero-scale",
                format!("`{name}` has a scale of 0, so no user value can be computed"),
                constant.line,
                "Constants",
            ));
        }
    }

    definition.diagnostics.extend(problems);
}

fn validate_references(definition: &mut Definition) {
    let mut problems = Vec::new();

    for (id, table) in definition.tables.iter() {
        // Axis and cell arrays live in either `[Constants]` or `[PcVariables]`:
        // rusEFI keeps its axis arrays as PC variables (they are user
        // preferences, not controller memory).
        let array_exists = |name: &str| {
            definition.constants.contains(name) || definition.pc_variables.contains(name)
        };

        for (axis_name, axis) in [("x", &table.x_bins), ("y", &table.y_bins)] {
            let Some(axis) = axis else { continue };
            if !array_exists(&axis.array) {
                problems.push(Diagnostic::new(
                    Severity::Warning,
                    "missing-bins-array",
                    format!(
                        "table `{id}` names {axis_name}-axis array `{}`, which no constant defines",
                        axis.array
                    ),
                    table.line,
                    "TableEditor",
                ));
            }
            if let Some(channel) = &axis.channel
                && !definition.channels.contains(channel)
            {
                problems.push(Diagnostic::new(
                    Severity::Note,
                    "missing-axis-channel",
                    format!(
                        "table `{id}` tracks channel `{channel}`, which is not an output channel"
                    ),
                    table.line,
                    "TableEditor",
                ));
            }
        }
        if let Some(z) = &table.z_bins
            && !array_exists(z)
        {
            problems.push(Diagnostic::new(
                Severity::Warning,
                "missing-table-array",
                format!("table `{id}` names cell array `{z}`, which no constant defines"),
                table.line,
                "TableEditor",
            ));
        }
    }

    definition.diagnostics.extend(problems);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> String {
        "[MegaTune]\nsignature = \"test 1\"\nqueryCommand = \"Q\"\n\n[Constants]\nnPages = 1\npageSize = 288\npage = 1\nx = scalar, U08, 0, \"ms\", 1.0, 0, 0, 255, 0\n".to_owned()
    }

    #[test]
    fn a_minimal_definition_parses_into_a_frozen_model() {
        let definition = parse_str(&minimal(), &SettingsContext::new()).expect("parses");

        assert_eq!(definition.signature, "test 1");
        assert_eq!(definition.signature_section, "MegaTune");
        assert_eq!(definition.pages.len(), 1);
        assert_eq!(definition.constants.len(), 1);
        assert_eq!(definition.constant("x").map(|c| c.page), Some(1));
    }

    #[test]
    fn a_file_without_a_signature_is_rejected() {
        let error = parse_str("[Constants]\npage = 1\n", &SettingsContext::new())
            .expect_err("must be rejected");

        assert_eq!(error.diagnostics()[0].code, "missing-signature");
    }

    #[test]
    fn a_newer_spec_version_fails_loudly() {
        let text = "[MegaTune]\nsignature = \"x\"\n[TunerStudio]\niniSpecVersion = 9.99\n";

        let error = parse_str(text, &SettingsContext::new()).expect_err("must be rejected");

        assert_eq!(error.diagnostics()[0].code, "unsupported-spec");
    }

    #[test]
    fn an_unknown_section_is_kept_as_data_not_an_error() {
        let text = format!("{}\n[EventTriggers]\n  trigger = something\n", minimal());

        let definition = parse_str(&text, &SettingsContext::new()).expect("parses");

        assert_eq!(definition.unsupported.len(), 1);
        assert_eq!(definition.unsupported[0].name, "EventTriggers");
        assert_eq!(definition.unsupported[0].lines, vec!["trigger = something"]);
    }

    #[test]
    fn an_entry_outside_its_page_is_an_error_with_a_line_number() {
        let text = "[MegaTune]\nsignature = \"x\"\n[Constants]\nnPages = 1\npageSize = 4\npage = 1\nwide = array, U16, 3, [4], \"x\", 1.0, 0\n";

        let definition = parse_str(text, &SettingsContext::new()).expect("parses");

        let error = definition
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "offset-outside-page")
            .expect("reported");
        assert_eq!(error.line, Some(7));
    }

    #[test]
    fn coverage_counts_are_per_section() {
        let definition = parse_str(&minimal(), &SettingsContext::new()).expect("parses");

        let constants = definition.coverage.section("Constants").expect("row");
        assert_eq!(constants.understood, constants.entries);
        assert_eq!(definition.coverage.percent(), 100.0);
    }
}
