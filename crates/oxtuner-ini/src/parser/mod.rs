//! Pass 2–4: sections → resolved model → frozen [`Definition`].
//!
//! Sections are dispatched in document order, line by line, because the
//! dialect is order sensitive: a constant referenced in another constant's
//! expression must be declared earlier, and `[PcVariables]` load before
//! `[Constants]` on purpose (reference §Section load order).
//!
//! A second header with the same name continues the open section builder. Its
//! lines are read where they sit in the document, not moved up to the first
//! block. That keeps order-sensitive readings honest: a block between the two
//! headers sees the model the first one left behind. The `merged-section` note
//! is written on the repeated header and names both header lines.
//!
//! Diagnostics are flushed after each block, and a sink's final work runs
//! after that section's last block, so the report follows source order instead
//! of being grouped by section name. Coverage is recorded once, in
//! first-appearance order.
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
mod sections;
mod validation;

use std::collections::{BTreeSet, HashMap, hash_map::Entry};

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

/// Drops this parse's literal intern table when `assemble` leaves, on both the
/// success and error paths. The `Arc<str>` values it handed out stay alive in
/// the returned definition.
struct TextScope;

impl Drop for TextScope {
    fn drop(&mut self) {
        crate::model::clear_text_cache();
    }
}

fn assemble(
    text: &str,
    settings: &SettingsContext,
    includes: Option<IncludeResolver<'_>>,
    extra_diagnostics: Vec<Diagnostic>,
) -> Result<Definition, IniError> {
    let _text_scope = TextScope;

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
    for name in expander.truncated_defines() {
        push_bounded(
            &mut definition.diagnostics,
            Diagnostic::whole_file(
                Severity::Warning,
                "fields-truncated",
                format!(
                    "`#define {name}` expands past the field cap; references to it are left unexpanded"
                ),
            ),
        );
    }

    let coverage = {
        let _span = tracing::debug_span!("ini_parse_sections").entered();
        parse_sections(&mut definition, &preprocessed.lines, &expander)
    };
    definition.coverage = coverage;

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

/// Dispatches every section block in document order and returns its coverage.
fn parse_sections(
    definition: &mut Definition,
    lines: &[LogicalLine],
    expander: &Expander<'_>,
) -> Coverage {
    let blocks = section_blocks(lines);

    // A sink's final work can itself emit diagnostics, so run it after that
    // section's last block rather than after every other section is fed. That
    // keeps those notes in source order too. Coverage is still recorded once,
    // in first-appearance order.
    let mut last_blocks: HashMap<String, usize> = HashMap::new();
    for (index, block) in blocks.iter().enumerate().rev() {
        last_blocks
            .entry(block.name.to_ascii_lowercase())
            .or_insert(index);
    }

    let mut coverage = Coverage::default();
    let mut slots: HashMap<String, SectionSlot> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for (block_index, block) in blocks.iter().enumerate() {
        let key = block.name.to_ascii_lowercase();
        let (slot, is_last_block) = match slots.entry(key) {
            Entry::Occupied(entry) => {
                let last = last_blocks.get(entry.key()).copied().unwrap_or(block_index);
                (entry.into_mut(), last == block_index)
            }
            Entry::Vacant(entry) => {
                let last = last_blocks.get(entry.key()).copied().unwrap_or(block_index);
                let slot = SectionSlot::new(block.name, block.header.unwrap_or(0), entry.key());
                order.push(entry.key().clone());
                (entry.insert(slot), last == block_index)
            }
        };

        if slot.headers >= 1 {
            slot.parser.note(
                "merged-section",
                format!(
                    "`[{}]` is declared again here; its entries continue the block at line {}",
                    block.name, slot.first_line
                ),
                block.header.unwrap_or(0),
            );
        }
        slot.headers += 1;

        slot.sink
            .feed(definition, &mut slot.parser, &block.lines, expander);
        slot.parser.flush_diagnostics(definition);

        if is_last_block {
            slot.sink.finish(definition, &mut slot.parser);
            slot.parser.flush_diagnostics(definition);
        }
    }

    for key in &order {
        slots
            .get(key)
            .expect("every ordered name has a slot")
            .parser
            .finish_coverage(&mut coverage);
    }

    coverage
}

/// One `[section]` header and the lines that follow it, in document order.
///
/// Text before the first header is the preamble and has no header line. Real
/// files use one block per section, but a repeated header gets its own block so
/// its lines are read at their document position. The slot below keeps the
/// builder alive across all blocks with the same name.
struct SectionBlock<'a> {
    name: &'a str,
    header: Option<u32>,
    lines: Vec<(u32, &'a str)>,
}

fn section_blocks(lines: &[LogicalLine]) -> Vec<SectionBlock<'_>> {
    let mut blocks = vec![SectionBlock {
        name: "",
        header: None,
        lines: Vec::new(),
    }];

    for line in lines {
        if let Some(name) = section_header(&line.text) {
            blocks.push(SectionBlock {
                name,
                header: Some(line.line),
                lines: Vec::new(),
            });
            continue;
        }
        blocks
            .last_mut()
            .expect("the preamble block is always present")
            .lines
            .push((line.line, line.text.as_str()));
    }

    blocks
}

fn section_header(text: &str) -> Option<&str> {
    let text = text.trim_ascii();
    let inner = text.strip_prefix('[')?.strip_suffix(']')?;
    Some(inner.trim_ascii())
}

/// The persistent builder for one section name.
struct SectionSlot {
    parser: SectionParser,
    sink: Box<dyn SectionSink>,
    headers: usize,
    first_line: u32,
}

impl SectionSlot {
    fn new(name: &str, first_line: u32, key: &str) -> Self {
        Self {
            parser: SectionParser::new(name),
            sink: new_sink(name, key),
            headers: 0,
            first_line,
        }
    }
}

/// The per-block half of a section parser. State that must survive a repeated
/// header lives here, not in the parser, so the dispatch loop can keep one
/// builder per section name.
pub(crate) trait SectionSink {
    fn feed(
        &mut self,
        definition: &mut Definition,
        section: &mut SectionParser,
        lines: &[(u32, &str)],
        expander: &Expander<'_>,
    );

    /// Runs once, after the last block with this section name.
    fn finish(&mut self, _definition: &mut Definition, _section: &mut SectionParser) {}
}

/// Adapts a section parser with no cross-block state to [`SectionSink`].
struct FnSink(fn(&mut Definition, &mut SectionParser, &[(u32, &str)], &Expander<'_>));

impl SectionSink for FnSink {
    fn feed(
        &mut self,
        definition: &mut Definition,
        section: &mut SectionParser,
        lines: &[(u32, &str)],
        expander: &Expander<'_>,
    ) {
        (self.0)(definition, section, lines, expander);
    }
}

/// An unknown section: raw lines accumulate across repeated headers and become
/// one [`UnsupportedSection`] at the end of the document.
struct UnknownSink {
    name: String,
    lines: Vec<String>,
}

impl SectionSink for UnknownSink {
    fn feed(
        &mut self,
        definition: &mut Definition,
        section: &mut SectionParser,
        lines: &[(u32, &str)],
        _expander: &Expander<'_>,
    ) {
        for (_, text) in lines {
            section.total = section.total.saturating_add(1);
            self.lines.push((*text).to_owned());
        }
        section.kept_raw = true;
        let _ = definition;
    }

    fn finish(&mut self, definition: &mut Definition, _section: &mut SectionParser) {
        definition
            .unsupported
            .push(crate::model::UnsupportedSection {
                name: self.name.clone(),
                lines: std::mem::take(&mut self.lines),
            });
    }
}

/// Preamble attributes before the first header (rusEFI has one).
fn parse_preamble(
    definition: &mut Definition,
    section: &mut SectionParser,
    lines: &[(u32, &str)],
    _expander: &Expander<'_>,
) {
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

/// Builds the sink for a section name. Stateless parsers share `FnSink`; only
/// builders with cross-block state get their own type.
fn new_sink(name: &str, key: &str) -> Box<dyn SectionSink> {
    match key {
        "constants" => Box::new(constants::ConstantsSink::default()),
        "pcvariables" => Box::new(constants::PcVariablesSink::default()),
        "tableeditor" => Box::new(editors::TablesSink::default()),
        "curveeditor" => Box::new(editors::CurvesSink::default()),
        "settinggroups" => Box::new(dialogs::SettingGroupsSink::default()),
        "userdefined" | "uidialogs" => Box::new(dialogs::DialogsSink::default()),
        "menu" => Box::new(dialogs::MenuSink::default()),
        "gaugeconfigurations" => Box::new(dashboard::GaugesSink::default()),
        "referencetables" => Box::new(misc::ReferenceTablesSink::default()),
        "datalog" => Box::new(misc::DatalogSink::default()),
        "loggerdefinition" => Box::new(sections::LoggersSink::default()),
        "porteditor" | "ftpbrowser" | "veanalyze" | "wueanalyze" | "gammae" | "datalogviews"
        | "tools" => Box::new(sections::ConfigSectionSink::new(name)),
        "" => Box::new(FnSink(parse_preamble)),
        "megatune" | "tunerstudio" => Box::new(FnSink(|d, s, l, _| {
            constants::parse_identification(d, s, l);
        })),
        "constantsextensions" | "constantextensions" | "defaults" => {
            Box::new(FnSink(|d, s, l, _| misc::parse_extensions(d, s, l)))
        }
        "outputchannels" => Box::new(FnSink(channels::parse_channels)),
        "burstmode" => Box::new(FnSink(|d, s, l, _| channels::parse_burst_mode(d, s, l))),
        "frontpage" => Box::new(FnSink(|d, s, l, _| dashboard::parse_front_page(d, s, l))),
        "settingcontexthelp" => Box::new(FnSink(|d, s, l, _| dialogs::parse_context_help(d, s, l))),
        "keyactions" => Box::new(FnSink(|d, s, l, _| dialogs::parse_key_actions(d, s, l))),
        "controllercommands" => Box::new(FnSink(|d, s, l, _| {
            misc::parse_controller_commands(d, s, l);
        })),
        _ => Box::new(UnknownSink {
            name: name.to_owned(),
            lines: Vec::new(),
        }),
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

    /// Moves this block's diagnostics into the file-wide report. Called after
    /// every block so the report follows source order instead of grouping by
    /// section name.
    fn flush_diagnostics(&mut self, definition: &mut Definition) {
        let diagnostics = std::mem::take(&mut self.diagnostics);
        extend_bounded(&mut definition.diagnostics, diagnostics);
    }

    /// Records coverage once the document is complete, using the totals
    /// accumulated across every block with this name.
    fn finish_coverage(&self, coverage: &mut Coverage) {
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
    resolve_comms_keys(definition);
    apply_extension_flags(definition);
    validation::promote_live_cursors(definition);
    validation::resolve_dynamic_shapes(definition);
    validate_pages(definition);
    validate_references(definition);
    validation::validate_expression_calls(definition);
}

/// Resolves communication keys that appear in more than one section.
///
/// A header declaration wins over a page-local one, whichever order the file
/// wrote them in. The note names the winning key spelling and both locations so
/// a changed `blockingFactor` can be traced without re-reading the whole file.
fn resolve_comms_keys(definition: &mut Definition) {
    let sources = std::mem::take(&mut definition.comms_sources);
    let mut notes = Vec::new();

    for (lower, candidates) in sources {
        // A header still outranks a page-local spelling, but the last live
        // declaration of the winning class wins, as the sequential reader did.
        let Some(winner) = candidates
            .iter()
            .rfind(|source| {
                source.section.eq_ignore_ascii_case("MegaTune")
                    || source.section.eq_ignore_ascii_case("TunerStudio")
            })
            .or_else(|| candidates.last())
        else {
            continue;
        };

        if candidates.len() > 1 {
            let loser = candidates
                .iter()
                .find(|source| !std::ptr::eq(*source, winner));
            let other = loser.map_or_else(
                || format!("{} other declaration(s)", candidates.len() - 1),
                |source| format!("[{}] line {}", source.section, source.line),
            );
            notes.push(Diagnostic::new(
                Severity::Note,
                "comms-key-winner",
                format!(
                    "`{}` from [{}] line {} wins over {other}",
                    winner.key, winner.section, winner.line
                ),
                winner.line,
                &winner.section,
            ));
        }

        definition
            .comms
            .raw
            .insert(lower.clone(), winner.value.clone());
        constants::apply_comms_value(definition, &lower, &winner.value);
    }

    extend_bounded(&mut definition.diagnostics, notes);
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

    // A table or curve may be completed by a later block, so these checks run
    // against the final model rather than a partially read one.
    for (_, table) in definition.tables.iter() {
        if table.z_bins.is_none() {
            problems.push(Diagnostic::new(
                Severity::Warning,
                "table-without-cells",
                format!("table `{}` has no `zBins` cell array", table.id),
                table.line,
                "TableEditor",
            ));
        }
    }

    for (_, curve) in definition.curves.iter() {
        if curve.x_bins.is_none() || curve.y_bins.is_empty() {
            problems.push(Diagnostic::new(
                Severity::Warning,
                "curve-without-arrays",
                format!(
                    "curve `{}` needs an X array and at least one Y array",
                    curve.name
                ),
                curve.line,
                "CurveEditor",
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

    fn parse(text: &str) -> Definition {
        parse_str(text, &SettingsContext::new()).expect("parses")
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
    fn a_split_section_reports_diagnostics_in_source_order() {
        let text = "\
[MegaTune]
signature = \"x\"
[Constants]
page = nope
[OutputChannels]
maxUnusedRuntimeRange = nope
[Constants]
page = nope
";

        let definition = parse_str(text, &SettingsContext::new()).expect("parses");

        // The second `[Constants]` block sits after `[OutputChannels]` in the
        // file, so its warning must be reported after the channel one. Grouping
        // diagnostics by section name used to produce [4, 8, 6].
        let lines: Vec<u32> = definition
            .diagnostics
            .iter()
            .filter(|diagnostic| matches!(diagnostic.code.as_ref(), "bad-page" | "bad-number"))
            .filter_map(|diagnostic| diagnostic.line)
            .collect();
        assert_eq!(lines, vec![4, 6, 8]);
    }

    #[test]
    fn a_final_table_merge_note_precedes_later_sections() {
        let text = "\
[MegaTune]
signature = \"x\"
[TableEditor]
table = t, m, \"T\"
table = t, m, \"T\"
[Constants]
page = nope
";

        let definition = parse_str(text, &SettingsContext::new()).expect("parses");

        // The merge note is emitted by the sink's final work, but its line is
        // before the constants warning. It must not be pushed behind it.
        let lines: Vec<u32> = definition
            .diagnostics
            .iter()
            .filter(|diagnostic| matches!(diagnostic.code.as_ref(), "merged-table" | "bad-page"))
            .filter_map(|diagnostic| diagnostic.line)
            .collect();
        assert_eq!(lines, vec![5, 7]);
    }

    #[test]
    fn a_split_constants_block_keeps_the_page_cursor() {
        let text = "\
[MegaTune]
signature = \"x\"
[Constants]
nPages = 1
pageSize = 64
page = 1
a = scalar, U08, 0, \"u\", 1, 0, 0, 255, 0
[OutputChannels]
c = scalar, U08, 0, \"u\", 1, 0
[Constants]
page = 1
b = scalar, U08, nextOffset, \"u\", 1, 0, 0, 255, 0
";

        let definition = parse_str(text, &SettingsContext::new()).expect("parses");

        // Restating `page = 1` continues the same page instead of starting a
        // new one, so `nextOffset` follows the cursor left by `a`.
        assert_eq!(
            definition
                .constant("b")
                .and_then(|constant| constant.address),
            Some(1)
        );
        assert!(
            !definition
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "offset-outside-page")
        );
    }

    #[test]
    fn a_split_constants_keeps_constant_order() {
        let text = "\
[MegaTune]
signature = \"x\"
[Constants]
nPages = 1
pageSize = 64
page = 1
a = scalar, U08, 0, \"u\", 1, 0, 0, 255, 0
[OutputChannels]
[Constants]
b = scalar, U08, nextOffset, \"u\", 1, 0, 0, 255, 0
";

        let definition = parse_str(text, &SettingsContext::new()).expect("parses");

        let names: Vec<&str> = definition.constants.iter().map(|(name, _)| name).collect();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(definition.constant("a").and_then(|c| c.address), Some(0));
        assert_eq!(definition.constant("b").and_then(|c| c.address), Some(1));
    }

    #[test]
    fn a_split_dialog_continues_its_row_list() {
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
        let fields: Vec<&str> = dialog
            .rows
            .iter()
            .map(|row| row.fields[1].as_str())
            .collect();
        assert_eq!(fields, ["a", "b", "c"]);

        let merged = definition
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "merged-section")
            .expect("the repeated header is reported");
        assert_eq!(merged.line, Some(6));
        assert!(
            merged
                .message
                .contains("its entries continue the block at line 3")
        );
    }

    #[test]
    fn a_split_table_editor_continues_the_same_table() {
        let text = "\
[MegaTune]
signature = \"x\"
[TableEditor]
table = t, m, \"T\"
xBins = x
[CurveEditor]
curve = c, \"C\"
xBins = cx
yBins = cy
[TableEditor]
table = t, m, \"T\"
yBins = y
zBins = z
";

        let definition = parse_str(text, &SettingsContext::new()).expect("parses");

        assert_eq!(definition.tables.len(), 1);
        let table = definition.tables.get("t").expect("the merged table");
        assert_eq!(
            table.x_bins.as_ref().map(|axis| axis.array.as_str()),
            Some("x")
        );
        assert_eq!(
            table.y_bins.as_ref().map(|axis| axis.array.as_str()),
            Some("y")
        );
        assert_eq!(table.z_bins.as_deref(), Some("z"));

        let merged = definition
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "merged-table")
            .expect("the repeated declaration is reported");
        assert_eq!(merged.line, Some(11));
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
    fn pure_builtins_fold_while_the_expression_source_is_kept() {
        let definition = parse(
            "\
[MegaTune]
signature = \"x\"
[Constants]
nPages = 1
pageSize = 64
page = 1
x = scalar, U08, 0, \"u\", { min(4, 2) }, 0, 0, 255, { abs(-3) }
",
        );

        let constant = definition.constant("x").expect("the constant parses");
        let scale = constant.scale.as_ref().expect("scale");
        assert_eq!(scale.evaluate(&|_| None), Some(2.0));
        assert_eq!(scale.normalized(), "2");
        assert_eq!(scale.to_string(), "{min(4, 2)}");
        let digits = constant.digits.as_ref().expect("digits");
        assert_eq!(digits.evaluate(&|_| None), Some(3.0));
    }

    #[test]
    fn an_unknown_call_keeps_its_source_and_gets_a_fold_deferred_note() {
        let definition = parse(
            "\
[MegaTune]
signature = \"x\"
[Constants]
nPages = 1
pageSize = 64
page = 1
x = scalar, U08, 0, \"u\", { someFutureBuiltin(1) }, 0, 0, 255, 1
",
        );

        let scale = definition
            .constant("x")
            .and_then(|constant| constant.scale.as_ref())
            .expect("scale");
        assert_eq!(scale.to_string(), "{someFutureBuiltin(1)}");
        let note = definition
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "fold-deferred")
            .expect("the unknown call is reported");
        assert!(note.message.contains("someFutureBuiltin"));
    }

    #[test]
    fn string_functions_parse_into_the_expression_ast() {
        let definition = parse(
            "\
[MegaTune]
signature = \"x\"
[Constants]
nPages = 1
pageSize = 64
page = 1
x = scalar, U08, 0, { bitStringValue(units, algo) }, 1, 0, 0, 255, 1
",
        );

        let units = &definition.constant("x").expect("the constant parses").units;
        let crate::model::Text::Expression {
            parsed: Some(crate::expr::Expr::Call(name, arguments)),
            ..
        } = units
        else {
            panic!("the string function is parsed as a call");
        };
        assert_eq!(name, "bitStringValue");
        assert_eq!(arguments.len(), 2);
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

    #[test]
    fn repeated_units_and_help_text_share_one_allocation() {
        let definition = parse(
            "\
[MegaTune]
signature = \"x\"
[Constants]
nPages = 1
pageSize = 64
page = 1
a = scalar, U08, 0, \"rpm\", 1, 0, 0, 255, 0
b = scalar, U08, 1, \"rpm\", 1, 0, 0, 255, 0
[SettingContextHelp]
a = \"Shared help\"
b = \"Shared help\"
",
        );

        let crate::model::Text::Literal(first) = &definition.constant("a").expect("a").units else {
            panic!("units are literal text");
        };
        let crate::model::Text::Literal(second) = &definition.constant("b").expect("b").units
        else {
            panic!("units are literal text");
        };
        assert!(std::sync::Arc::ptr_eq(first, second));

        let first_help = definition.context_help.get("a").expect("a help");
        let second_help = definition.context_help.get("b").expect("b help");
        assert!(std::sync::Arc::ptr_eq(first_help, second_help));
    }

    #[test]
    fn a_header_communication_key_wins_over_a_page_local_spelling() {
        let definition = parse(
            "\
[MegaTune]
signature = \"x\"
blockingFactor = 240
[Constants]
nPages = 1
pageSize = 288
page = 1
blockingFactor = 251
x = scalar, U08, 0, \"u\", 1, 0, 0, 255, 0
",
        );

        assert_eq!(definition.comms.blocking_factor, Some(240));
        assert_eq!(
            definition
                .comms
                .raw
                .get("blockingfactor")
                .map(String::as_str),
            Some("240")
        );
        let note = definition
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "comms-key-winner")
            .expect("the conflicting spellings are reported");
        assert!(note.message.contains("blockingFactor"));
        assert_eq!(note.line, Some(3));
    }

    #[test]
    fn the_last_declaration_wins_within_one_comms_section() {
        let definition = parse(
            "\
[MegaTune]
signature = \"x\"
[Constants]
nPages = 1
pageSize = 288
page = 1
blockingFactor = 99
blockingFactor = 77
x = scalar, U08, 0, \"u\", 1, 0, 0, 255, 0
",
        );

        assert_eq!(definition.comms.blocking_factor, Some(77));
        assert_eq!(
            definition
                .comms
                .raw
                .get("blockingfactor")
                .map(String::as_str),
            Some("77")
        );
    }

    #[test]
    fn a_logger_definition_keeps_bit_fields_and_symbolic_conditions() {
        let definition = parse(
            "\
[MegaTune]
signature = \"x\"
[LoggerDefinition]
loggerDef = tooth, \"Tooth Logger\", tooth
 startCommand = \"H\"
 stopCommand = \"h\"
 dataReadCommand = \"T\"
 dataReadyCondition = { toothLog1Ready == 1 }
 recordDef = 0, 0, 4
 recordField = toothTime, \"ToothTime\", 0, 32, 1.0, \"uS\"
 calcField = maxTime, \"MaxTime\", \"ms\", { maxValue(refTime) }, hidden
",
        );

        assert_eq!(definition.loggers.len(), 1);
        let logger = &definition.loggers[0];
        assert_eq!(logger.name, "tooth");
        assert_eq!(logger.kind, "tooth");
        assert_eq!(logger.header_len, 0);
        assert_eq!(logger.record_len, 4);
        assert_eq!(logger.start_command.as_deref(), Some("H"));
        assert_eq!(
            logger.data_ready_condition.as_deref(),
            Some("{ toothLog1Ready == 1 }")
        );
        assert_eq!(logger.fields.len(), 1);
        assert_eq!(logger.fields[0].name, "toothTime");
        assert_eq!(logger.fields[0].start_bit, Some(0));
        assert_eq!(logger.fields[0].bit_count, Some(32));
        assert_eq!(logger.fields[0].units.literal(), Some("uS"));
        assert_eq!(logger.calculated.len(), 1);
        assert!(logger.calculated[0].hidden);
    }

    #[test]
    fn named_configuration_sections_keep_typed_entries() {
        let definition = parse(
            "\
[MegaTune]
signature = \"x\"
[PortEditor]
pins = 1, 2, 3
 watched = { rpm }, >, 3000
[FTPBrowser]
host = \"192.168.0.1\"
 port = 21
",
        );

        assert_eq!(definition.config_sections.len(), 2);
        let ports = &definition.config_sections[0];
        assert_eq!(ports.name, "PortEditor");
        assert_eq!(ports.entries.len(), 2);
        assert_eq!(ports.entries[0].fields, vec!["1", "2", "3"]);
        assert_eq!(ports.entries[1].fields[1], ">");
        assert_eq!(definition.config_sections[1].name, "FTPBrowser");
        assert_eq!(definition.unsupported.len(), 0);
    }

    #[test]
    fn unknown_standard_menu_targets_are_listed_and_marked_unsupported() {
        let definition = parse(
            "\
[MegaTune]
signature = \"x\"
[Menu]
menu = \"Tools\"
subMenu = std_realtime, \"Realtime\"
subMenu = std_ms3SdConsole, \"SD Console\"
",
        );

        assert_eq!(definition.unresolved_targets, vec!["std_ms3SdConsole"]);
        let menu = definition.menus.first().expect("the menu");
        assert!(menu.items[0].supported);
        assert!(!menu.items[1].supported);
        assert_eq!(menu.items[1].kind, crate::MenuTargetKind::Standard);
    }

    #[test]
    fn a_table_and_curve_promote_their_axis_channels_as_live_cursors() {
        let definition = parse(
            "\
[MegaTune]
signature = \"x\"
[TableEditor]
table = t, m, \"T\"
xBins = rpmBins, rpm
yBins = mapBins, map
zBins = veTable
[CurveEditor]
curve = c, \"C\", rpm, map
yBins = curveBins, firstChannel
yBins = otherBins, secondChannel
",
        );

        let table = definition.table("t").expect("the table");
        assert_eq!(table.live_cursor.as_deref(), Some("rpm"));
        let curve = definition.curves.get("c").expect("the curve");
        assert_eq!(curve.live_cursor.as_deref(), Some("firstChannel"));
    }
}
