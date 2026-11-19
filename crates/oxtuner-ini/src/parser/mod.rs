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

use std::collections::{BTreeSet, HashMap};

use crate::error::{Diagnostic, IniError, Severity, extend_bounded, push_bounded};
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
/// A released build bumps this whenever the same input bytes and settings can
/// produce a different `Definition` (a grammar fix, a new section, a changed
/// default). The definition cache includes it in its key, so an upgrade misses
/// instead of serving a definition the build no longer agrees with. Before the
/// first release it stays at 1, so a cache entry written by an older build is
/// not invalidated by a bump. Delete the cache file and let the app re-parse.
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
    extend_bounded(&mut definition.diagnostics, extra_diagnostics);
    extend_bounded(
        &mut definition.diagnostics,
        preprocessed.diagnostics.iter().cloned(),
    );

    let expander = Expander::new(&preprocessed.defines);

    {
        let _span = tracing::debug_span!("ini_parse_sections").entered();
        let sections = split_sections(&preprocessed.lines);

        let mut coverage = Coverage::default();
        for section in &sections {
            let mut parser = SectionParser::new(&section.name);
            if section.repeats > 0 {
                parser.note_section(
                    "merged-section",
                    format!(
                        "`[{}]` has {} header(s); their entries are merged into one section",
                        section.name,
                        section.repeats + 1
                    ),
                );
            }
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
    /// Extra headers with this name folded into this one. Real files use one
    /// block per section, but a second one must continue the first builder
    /// instead of throwing its state away.
    repeats: usize,
}

fn split_sections(lines: &[LogicalLine]) -> Vec<RawSection<'_>> {
    let mut sections: Vec<RawSection<'_>> = vec![RawSection {
        name: String::new(),
        lines: Vec::new(),
        repeats: 0,
    }];
    let mut by_name: HashMap<&str, usize> = HashMap::new();
    let mut current = 0usize;

    for line in lines {
        if let Some(name) = section_header(&line.text) {
            if let Some(&index) = by_name.get(name) {
                sections[index].repeats += 1;
                current = index;
            } else {
                let index = sections.len();
                by_name.insert(name, index);
                sections.push(RawSection {
                    name: name.to_owned(),
                    lines: Vec::new(),
                    repeats: 0,
                });
                current = index;
            }
            continue;
        }
        sections[current]
            .lines
            .push((line.line, line.text.as_str()));
    }

    sections
}

fn section_header(text: &str) -> Option<&str> {
    let text = text.trim_ascii();
    let inner = text.strip_prefix('[')?.strip_suffix(']')?;
    Some(inner.trim_ascii())
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
                    definition.comms.raw.insert(
                        key.trim_ascii().to_ascii_lowercase(),
                        value.trim_ascii().to_owned(),
                    );
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

    /// Adds one diagnostic through the file-wide bound.
    fn record(&mut self, diagnostic: Diagnostic) {
        push_bounded(&mut self.diagnostics, diagnostic);
    }

    /// Records a warning tied to a line.
    pub(crate) fn warn(&mut self, code: &'static str, message: impl Into<String>, line: u32) {
        self.record(Diagnostic::new(
            Severity::Warning,
            code,
            message,
            line,
            &self.name,
        ));
    }

    /// Records an informational note tied to a line.
    pub(crate) fn note(&mut self, code: &'static str, message: impl Into<String>, line: u32) {
        self.record(Diagnostic::new(
            Severity::Note,
            code,
            message,
            line,
            &self.name,
        ));
    }

    /// Records an informational note about the section as a whole, for a fact
    /// that belongs to no single line.
    pub(crate) fn note_section(&mut self, code: &'static str, message: impl Into<String>) {
        self.record(Diagnostic::section_wide(
            Severity::Note,
            code,
            message,
            &self.name,
        ));
    }

    /// Records an error tied to a line.
    pub(crate) fn error(&mut self, code: &'static str, message: impl Into<String>, line: u32) {
        self.record(Diagnostic::new(
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
            format!("unrecognized line `{}`", text.trim_ascii()),
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
        extend_bounded(&mut definition.diagnostics, self.diagnostics);
    }
}

/// Convenience used by every section parser: count the entry, then decide.
pub(crate) fn entry_line<'a>(
    section: &mut SectionParser,
    line: u32,
    text: &'a str,
) -> Option<(&'a str, &'a str)> {
    section.count_entry();
    if let Some((key, value)) = crate::fields::split_assignment(text) {
        Some((key, value))
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
    //
    // The same lists also name PC variables, so both collections are walked.
    let Definition {
        extensions,
        constants,
        pc_variables,
        ..
    } = definition;

    for (name, constant) in constants.iter_mut() {
        if extensions.is_read_only(name) || extensions.is_controller_priority(name) {
            constant.flags.controller_priority = true;
        }
    }

    for (name, variable) in pc_variables.iter_mut() {
        if extensions.is_read_only(name) || extensions.is_controller_priority(name) {
            variable.flags.controller_priority = true;
        }
    }
}

fn validate_pages(definition: &mut Definition) {
    let mut problems = Vec::new();

    for (name, constant) in definition.constants.iter() {
        if constant.byte_size().is_none() && !constant.shape.is_dynamic() {
            let unit = if constant.shape.element_count().is_none() {
                "elements"
            } else {
                "bytes"
            };
            problems.push(Diagnostic::new(
                Severity::Error,
                "shape-too-large",
                format!("`{name}` declares more {unit} than a 32-bit size can hold"),
                constant.line,
                "Constants",
            ));
        }

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

    for (name, variable) in definition.pc_variables.iter() {
        // PC variables never become controller memory, so only the element
        // count has to be representable. There is no byte size to exceed.
        if variable.shape.element_count().is_none() && !variable.shape.is_dynamic() {
            problems.push(Diagnostic::new(
                Severity::Error,
                "shape-too-large",
                format!("`{name}` declares more elements than a 32-bit size can hold"),
                variable.line,
                "PcVariables",
            ));
        }
    }

    extend_bounded(&mut definition.diagnostics, problems);
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

    extend_bounded(&mut definition.diagnostics, problems);
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
    fn a_quoted_bit_option_keeps_the_equals_signs_in_its_label() {
        let text = "[MegaTune]\nsignature = \"x\"\n[Constants]\nnPages = 1\npageSize = 288\n\
                    page = 1\n\
                    #define comparator_def = \"== (equal)\",  \"!= (different)\",  \
                    \">= (greater/equal)\"\n\
                    compare = bits, U08, 82, [0:2], $comparator_def\n";

        let definition = parse_str(text, &SettingsContext::new()).expect("parses");

        let labels: Vec<&str> = definition
            .constant("compare")
            .expect("the constant parses")
            .options
            .iter()
            .map(|option| option.label.as_str())
            .collect();
        assert_eq!(
            labels,
            ["== (equal)", "!= (different)", ">= (greater/equal)"],
            "an `=` inside a quoted label is part of the label, not an index"
        );
    }

    #[test]
    fn a_label_list_separated_by_periods_keeps_its_option_indexes() {
        // The reference Speeduino definition writes the first three labels of
        // `secondarySerialProtocol` as
        // `"Generic (Fixed List)". "Generic (ini File)". "CAN"`.
        let text = "[MegaTune]\nsignature = \"x\"\n[Constants]\nnPages = 1\npageSize = 64\n\
                    page = 1\n\
                    serial = bits, U08, 0, [3:6], \"Generic (Fixed List)\"\
                    . \"Generic (ini File)\". \"CAN\", \"msDroid\"\n";

        let definition = parse_str(text, &SettingsContext::new()).expect("parses");

        let options: Vec<(&str, u32)> = definition
            .constant("serial")
            .expect("the constant parses")
            .options
            .iter()
            .map(|option| (option.label.as_str(), option.value))
            .collect();
        assert_eq!(
            options,
            [
                ("Generic (Fixed List)", 0),
                ("Generic (ini File)", 1),
                ("CAN", 2),
                ("msDroid", 3),
            ]
        );
    }

    #[test]
    fn a_scaled_pc_variable_keeps_its_numeric_attributes() {
        let text = "[MegaTune]\nsignature = \"x\"\n[PcVariables]\n\
                    wueAFR = array, S16, [10], \"AFR\", 0.1, 0.0, -4.0, 4.0, 1\n";

        let definition = parse_str(text, &SettingsContext::new()).expect("parses");

        let variable = definition
            .pc_variables
            .get("wueAFR")
            .expect("the variable parses");
        let literal =
            |number: &Option<crate::Number>| number.as_ref().and_then(crate::Number::literal);
        assert_eq!(literal(&variable.scale), Some(0.1));
        assert_eq!(literal(&variable.translate), Some(0.0));
        assert_eq!(literal(&variable.minimum), Some(-4.0));
        assert_eq!(literal(&variable.maximum), Some(4.0));
        assert_eq!(literal(&variable.digits), Some(1.0));
    }

    #[test]
    fn a_pc_scalar_with_an_offset_keeps_its_units() {
        let text = "[MegaTune]\nsignature = \"x\"\n[PcVariables]\n\
                    v = scalar, U16, 2, \"u\", 1.0, 0.0\n";

        let definition = parse_str(text, &SettingsContext::new()).expect("parses");

        let variable = definition
            .pc_variables
            .get("v")
            .expect("the variable parses");
        assert_eq!(variable.units.literal(), Some("u"));
        assert_eq!(
            variable.scale.as_ref().and_then(crate::Number::literal),
            Some(1.0)
        );
        assert!(
            !definition
                .diagnostics
                .iter()
                .any(|issue| issue.code == "pc-offset-guess"),
            "an unambiguous offset needs no warning"
        );
    }

    #[test]
    fn a_pc_bits_variable_with_an_offset_keeps_its_options() {
        let text = "[MegaTune]\nsignature = \"x\"\n[PcVariables]\n\
                    v = bits, U08, 2, [0:1], \"Off\", \"On\"\n";

        let definition = parse_str(text, &SettingsContext::new()).expect("parses");

        let variable = definition
            .pc_variables
            .get("v")
            .expect("the variable parses");
        assert!(matches!(
            variable.shape,
            crate::Shape::Bits {
                low: 0,
                high: 1,
                ..
            }
        ));
        assert_eq!(variable.options.len(), 2);
    }

    #[test]
    fn two_numeric_fields_before_the_units_warn_about_the_offset_guess() {
        let text = "[MegaTune]\nsignature = \"x\"\n[PcVariables]\n\
                    v = scalar, U16, 2, 7, 1.0, 0.0\n";

        let definition = parse_str(text, &SettingsContext::new()).expect("parses");

        assert!(
            definition
                .diagnostics
                .iter()
                .any(|issue| issue.code == "pc-offset-guess"),
            "an ambiguous entry is reported instead of guessed in silence"
        );
    }

    #[test]
    fn the_extension_lists_reach_pc_variables_too() {
        let text = "[MegaTune]\nsignature = \"x\"\n[PcVariables]\n\
                    gearCount = scalar, U08, 0, \"\", 1.0, 0.0\n\
                    bias = scalar, U08, 1, \"\", 1.0, 0.0\n\
                    [ConstantsExtensions]\n\
                    readOnly = gearcount\n\
                    controllerPriority = BIAS\n";

        let definition = parse_str(text, &SettingsContext::new()).expect("parses");

        assert!(
            definition
                .pc_variables
                .get("gearCount")
                .is_some_and(|variable| variable.flags.controller_priority),
            "readOnly names a PC variable in another case"
        );
        assert!(
            definition
                .pc_variables
                .get("bias")
                .is_some_and(|variable| variable.flags.controller_priority),
            "controllerPriority names a PC variable"
        );
    }

    #[test]
    fn a_storage_keyword_after_a_bit_list_is_a_flag_not_an_option() {
        let text = "[MegaTune]\nsignature = \"x\"\n[Constants]\nnPages = 1\npageSize = 64\n\
                    page = 1\n\
                    field = bits, U08, 0, [0:2], \"Off\", \"On\", noMsqSave\n";

        let definition = parse_str(text, &SettingsContext::new()).expect("parses");

        let field = definition.constant("field").expect("the constant parses");
        assert_eq!(field.options.len(), 2, "the keyword is not an option");
        assert!(field.flags.no_msq_save);
    }

    #[test]
    fn a_storage_keyword_on_a_pc_variable_is_a_flag() {
        let text = "[MegaTune]\nsignature = \"x\"\n[PcVariables]\n\
                    bits_value = bits, U08, [0:1], \"Off\", \"On\", noMsqSave\n\
                    alias = string, ASCII, 8, noMsqSave\n";

        let definition = parse_str(text, &SettingsContext::new()).expect("parses");

        let bits_value = definition
            .pc_variables
            .get("bits_value")
            .expect("the bits variable parses");
        assert_eq!(bits_value.options.len(), 2, "the keyword is not an option");
        assert!(bits_value.flags.no_msq_save);

        let alias = definition
            .pc_variables
            .get("alias")
            .expect("the string variable parses");
        assert!(alias.flags.no_msq_save);
    }

    #[test]
    fn a_string_pc_variable_keeps_its_length() {
        let text = "[MegaTune]\nsignature = \"x\"\n[PcVariables]\nAlias = string, ASCII, 20\n";

        let definition = parse_str(text, &SettingsContext::new()).expect("parses");

        let variable = definition
            .pc_variables
            .get("Alias")
            .expect("the variable parses");
        assert_eq!(
            variable.shape,
            crate::Shape::Text {
                length: crate::Dimension::Fixed(20),
                columns: None,
            },
            "a string PC variable is text, not a scalar to parse as a number"
        );
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
    fn a_definition_with_a_deeply_nested_condition_returns_an_error() {
        let condition = format!("{}FLAG{}", "(".repeat(10_000), ")".repeat(10_000));
        let text = format!("[MegaTune]\nsignature = \"x\"\n#if {condition}\n#endif\n");

        let error = parse_str(&text, &SettingsContext::new()).expect_err("must be rejected");

        assert_eq!(error.diagnostics()[0].code, "condition-too-deep");
    }

    #[test]
    fn a_deeply_nested_field_is_reported_without_aborting() {
        let expression = format!("{}1{}", "(".repeat(1_000), ")".repeat(1_000));
        let text = format!(
            "[MegaTune]\nsignature = \"x\"\n[OutputChannels]\nderived = {{ {expression} }}, \"u\"\n"
        );

        let definition = parse_str(&text, &SettingsContext::new()).expect("parses");

        let diagnostic = definition
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "bad-channel")
            .expect("the deep field is reported");
        assert!(diagnostic.message.contains("nests too deeply"));
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
    fn an_overflowing_table_shape_has_no_size_and_is_reported() {
        // 65 536 x 65 536 is 2^32, one past what a u32 element count can hold.
        let text = "[MegaTune]\nsignature = \"x\"\n[Constants]\nnPages = 1\npageSize = 1024\npage = 1\nx = array, U08, 0, [65536x65536], \"x\", 1.0, 0\n";

        let definition = parse_str(text, &SettingsContext::new()).expect("parses");

        let constant = definition.constant("x").expect("x");
        assert_eq!(constant.shape.element_count(), None);
        assert!(
            !constant.shape.is_dynamic(),
            "the dimensions are fixed; only the product overflows"
        );
        assert_eq!(constant.byte_size(), None);

        let diagnostic = definition
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "shape-too-large")
            .expect("an unrepresentable shape must be reported, not silently accepted");
        assert_eq!(diagnostic.severity, Severity::Error);
        assert_eq!(diagnostic.line, Some(7));
        assert!(diagnostic.message.contains("more elements than"));
    }

    #[test]
    fn a_constant_whose_byte_size_overflows_is_reported_in_bytes() {
        // 2 200 000 000 elements of U16 is 4.4e9 bytes, past a u32 byte count,
        // while the element count itself still fits.
        let text = "[MegaTune]\nsignature = \"x\"\n[Constants]\nnPages = 1\npageSize = 1024\npage = 1\nx = array, U16, 0, [2200000000], \"x\", 1.0, 0\n";

        let definition = parse_str(text, &SettingsContext::new()).expect("parses");

        let constant = definition.constant("x").expect("x");
        assert!(constant.shape.element_count().is_some());
        assert_eq!(constant.byte_size(), None);

        let diagnostic = definition
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "shape-too-large")
            .expect("reported");
        assert!(diagnostic.message.contains("more bytes than"));
    }

    #[test]
    fn an_overflowing_pc_variable_shape_is_reported() {
        let text = "[MegaTune]\nsignature = \"x\"\n[PcVariables]\nx = array, U08, [65536x65536], \"x\", 1.0, 0\n";

        let definition = parse_str(text, &SettingsContext::new()).expect("parses");

        let diagnostic = definition
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "shape-too-large")
            .expect("reported");
        assert_eq!(diagnostic.severity, Severity::Error);
        assert_eq!(diagnostic.section.as_deref(), Some("PcVariables"));
    }

    #[test]
    fn coverage_counts_are_per_section() {
        let definition = parse_str(&minimal(), &SettingsContext::new()).expect("parses");

        let constants = definition.coverage.section("Constants").expect("row");
        assert_eq!(constants.understood, constants.entries);
        assert_eq!(definition.coverage.percent(), 100.0);
    }

    #[test]
    fn a_page_count_mismatch_has_no_line_and_keeps_its_section() {
        let text =
            "[MegaTune]\nsignature = \"x\"\n[Constants]\nnPages = 2\npageSize = 288\npage = 1\n";

        let definition = parse_str(text, &SettingsContext::new()).expect("parses");

        let diagnostic = definition
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "page-size-count")
            .expect("the mismatch is reported");
        assert_eq!(diagnostic.line, None);
        assert_eq!(diagnostic.section.as_deref(), Some("Constants"));
        assert!(!diagnostic.to_string().contains("0: "));
    }

    #[test]
    fn a_repeated_table_or_curve_header_merges_and_names_the_line() {
        let text = "\
[MegaTune]
signature = \"x\"

[TableEditor]
table = t, m, \"T\"
zBins = z
table = t, m, \"T\"
zBins = z

[CurveEditor]
curve = c, \"C\"
xBins = x
yBins = y
curve = c, \"C\"
xBins = x
yBins = y
";

        let definition = parse_str(text, &SettingsContext::new()).expect("parses");

        assert_eq!(definition.tables.len(), 1);
        assert_eq!(definition.curves.len(), 1);
        let lines: Vec<Option<u32>> = definition
            .diagnostics
            .iter()
            .filter(|diagnostic| {
                matches!(diagnostic.code.as_ref(), "merged-table" | "merged-curve")
            })
            .map(|diagnostic| diagnostic.line)
            .collect();
        assert_eq!(lines, vec![Some(7), Some(14)]);
    }

    #[test]
    fn doubled_constants_sections_merge_into_one_page_table() {
        let text = "\
[MegaTune]
signature = \"x\"
[Constants]
nPages = 1
pageSize = 64
page = 1
a = scalar, U08, 0, \"u\", 1, 0, 0, 255, 0
[Constants]
page = 1
b = scalar, U08, 1, \"u\", 1, 0, 0, 255, 0
";

        let definition = parse_str(text, &SettingsContext::new()).expect("parses");

        assert_eq!(definition.pages.len(), 1);
        assert_eq!(definition.constants.len(), 2);
        assert_eq!(definition.constant("a").and_then(|c| c.address), Some(0));
        assert_eq!(definition.constant("b").and_then(|c| c.address), Some(1));
        assert!(
            definition
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "merged-section")
        );
        assert!(
            !definition
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "offset-outside-page")
        );
    }

    #[test]
    fn doubled_dialog_sections_continue_the_same_dialog() {
        let text = "\
[MegaTune]
signature = \"x\"
[UiDialogs]
dialog = d, \"Title\", yAxis
 field = \"A\", a
[UiDialogs]
dialog = d, \"Title 2\", yAxis
 field = \"B\", b
 field = \"C\", c
";

        let definition = parse_str(text, &SettingsContext::new()).expect("parses");

        assert_eq!(definition.dialogs.len(), 1);
        let dialog = definition.dialogs.get("d").expect("the merged dialog");
        assert_eq!(dialog.title.as_deref(), Some("Title 2"));
        assert_eq!(dialog.rows.len(), 3);
        assert_eq!(dialog.rows[0].fields[1], "a");
        assert_eq!(dialog.rows[1].fields[1], "b");
        assert_eq!(dialog.rows[2].fields[1], "c");
        assert!(
            !definition
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "duplicate-name")
        );
        assert!(
            definition
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "merged-dialog")
        );
    }

    #[test]
    fn a_bad_offset_poisons_derived_offsets_until_an_explicit_one() {
        let text = "\
[MegaTune]
signature = \"x\"
[Constants]
nPages = 1
pageSize = 256
page = 1
bad = scalar, U08, bad@offset, \"u\", 1, 0, 0, 255, 0
derived = scalar, U08, nextOffset, \"u\", 1, 0, 0, 255, 0
fixed = scalar, U08, 10, \"u\", 1, 0, 0, 255, 0
after = scalar, U08, nextOffset, \"u\", 1, 0, 0, 255, 0
";

        let definition = parse_str(text, &SettingsContext::new()).expect("parses");

        let address = |name| definition.constant(name).and_then(|c| c.address);
        assert_eq!(address("bad"), None);
        assert_eq!(address("derived"), None);
        assert_eq!(address("fixed"), Some(10));
        assert_eq!(address("after"), Some(11));
        assert_eq!(
            definition.constant("derived").map(|c| &c.offset),
            Some(&crate::Offset::Unknown)
        );
        let bad_offsets: Vec<u32> = definition
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "bad-offset")
            .filter_map(|diagnostic| diagnostic.line)
            .collect();
        assert_eq!(bad_offsets, vec![7, 8]);
    }

    #[test]
    fn an_unquoted_units_field_is_warned_about() {
        let text = "[MegaTune]\nsignature = \"x\"\n[Constants]\nnPages = 1\npageSize = 288\npage = 1\nx = scalar, U08, 0, ms, 1.0, 0, 0, 255, 0\n";

        let definition = parse_str(text, &SettingsContext::new()).expect("parses");

        let diagnostic = definition
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "unquoted-units")
            .expect("the shifted field is reported");
        assert_eq!(diagnostic.severity, Severity::Warning);
        assert_eq!(diagnostic.line, Some(7));
        assert!(diagnostic.message.contains("\"ms\""));
    }

    #[test]
    fn an_include_that_cannot_be_resolved_is_reported_on_the_definition() {
        let text = format!("{}\n#include missing.inc\n", minimal());
        let resolver = |_: &str| None;

        let definition =
            parse_str_with_includes(&text, &SettingsContext::new(), &resolver).expect("parses");

        let diagnostic = definition
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "include-not-found")
            .expect("the unresolved include reaches the frozen definition");
        assert_eq!(diagnostic.severity, Severity::Note);
        assert_eq!(diagnostic.line, Some(11));
        assert!(diagnostic.message.contains("missing.inc"));
    }
}
