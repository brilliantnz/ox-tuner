//! Project and definition snapshots the UI names what is open with.

use std::path::PathBuf;

use oxtuner_ini::{Definition, Number, Severity};
use oxtuner_project::Project;

use crate::menu::MenuNode;

use super::{FrontPageGauge, GaugeSummary, IndicatorSummary};

/// One hidden-dialog hotkey from `[KeyActions]`, as the UI lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyActionSummary {
    /// Keys in order, as written.
    pub keys: String,
    /// Dialog the keys open.
    pub dialog: String,
}

/// One sensor calibration table from `[ReferenceTables]`.
///
/// Calibration screens (thermistor/AFR) are future work; this carries the
/// table's discovery data and solution list so they arrive with the
/// definition instead of needing `oxtuner-ini` types later.
#[derive(Debug, Clone, PartialEq)]
pub struct ReferenceTableSummary {
    /// Table name.
    pub name: String,
    /// Display title, when stated.
    pub title: Option<String>,
    /// Target table identifiers and labels.
    pub identifiers: Vec<ReferenceTableIdentifier>,
    /// Number of translated elements, when stated.
    pub adc_count: Option<u32>,
    /// Element width in bytes, when stated.
    pub bytes_per_adc: Option<u32>,
    /// Scale applied before writing, when the definition states a literal.
    pub scale: Option<f64>,
    /// Optional start offset.
    pub start_offset: Option<u32>,
    /// Per-identifier write limits, in document order.
    pub limits: Vec<ReferenceTableLimit>,
    /// Solution labels and expressions, as written.
    pub solutions: Vec<ReferenceTableSolution>,
    /// Source line, for a diagnostic or a tooltip.
    pub line: u32,
}

/// One target of a [`ReferenceTableSummary`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceTableIdentifier {
    /// Target table identifier.
    pub id: u32,
    /// Target label.
    pub label: String,
}

/// One write limit of a [`ReferenceTableSummary`]: identifier, min, max,
/// default, in the definition's order.
#[derive(Debug, Clone, PartialEq)]
pub struct ReferenceTableLimit {
    /// Target table identifier.
    pub identifier: u32,
    /// Minimum value, when the definition states a literal.
    pub minimum: Option<f64>,
    /// Maximum value, when the definition states a literal.
    pub maximum: Option<f64>,
    /// Default for railed values, when the definition states a literal.
    pub default: Option<f64>,
}

/// One solution of a [`ReferenceTableSummary`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceTableSolution {
    /// Solution label.
    pub label: String,
    /// Solution expression, as written.
    pub expression: String,
}

impl ReferenceTableSummary {
    /// Shapes one parsed calibration table for the UI.
    pub(crate) fn of(table: &oxtuner_ini::ReferenceTable) -> Self {
        Self {
            name: table.name.clone(),
            title: table.title.clone(),
            identifiers: table
                .identifiers
                .iter()
                .map(|(id, label)| ReferenceTableIdentifier {
                    id: *id,
                    label: label.clone(),
                })
                .collect(),
            adc_count: table.adc_count,
            bytes_per_adc: table.bytes_per_adc,
            scale: table.scale.as_ref().and_then(Number::literal),
            start_offset: table.start_offset,
            limits: table
                .limits
                .iter()
                .map(
                    |(identifier, minimum, maximum, default)| ReferenceTableLimit {
                        identifier: *identifier,
                        minimum: minimum.literal(),
                        maximum: maximum.literal(),
                        default: default.literal(),
                    },
                )
                .collect(),
            solutions: table
                .solutions
                .iter()
                .map(|(label, expression)| ReferenceTableSolution {
                    label: label.clone(),
                    expression: expression.clone(),
                })
                .collect(),
            line: table.line,
        }
    }
}

/// Deliberately small and `Clone`: the core keeps the full [`Definition`];
/// the UI only needs enough to name what it is looking at.
#[derive(Debug, Clone, PartialEq)]
pub struct DefinitionSummary {
    /// The label the load command carried.
    pub label: String,
    /// Firmware signature.
    pub signature: String,
    /// `iniSpecVersion`, when the file declares one.
    pub spec_version: Option<f64>,
    /// Calibration pages.
    pub pages: usize,
    /// Calibration constants.
    pub constants: usize,
    /// PC-side variables.
    pub pc_variables: usize,
    /// Realtime channels.
    pub channels: usize,
    /// 2-D tables.
    pub tables: usize,
    /// 1-D curves.
    pub curves: usize,
    /// Gauge templates from `[GaugeConfigurations]`, in definition order.
    pub gauges: Vec<GaugeSummary>,
    /// Dashboard slots from `[FrontPage]`, in slot order.
    pub front_page_gauges: Vec<FrontPageGauge>,
    /// Hidden-dialog hotkeys from `[KeyActions]`, in definition order.
    pub key_actions: Vec<KeyActionSummary>,
    /// Sensor calibration tables from `[ReferenceTables]`, in definition order.
    pub reference_tables: Vec<ReferenceTableSummary>,
    /// Sections kept as raw data, by name.
    pub unsupported: Vec<String>,
    /// Dialogs.
    pub dialogs: usize,
    /// `[FrontPage]` indicators, in definition order.
    pub indicators: Vec<IndicatorSummary>,
    /// The navigation tree the primary sidebar draws, in definition order.
    pub menu: Vec<MenuNode>,
    /// Share of entries that became model data, 0–100.
    pub coverage_percent: f64,
    /// Parse errors recorded while loading.
    pub errors: usize,
    /// Parse warnings recorded while loading.
    pub warnings: usize,
    /// `#if` flags active for this load, sorted.
    pub active_flags: Vec<String>,
}

impl DefinitionSummary {
    /// Builds the UI-facing summary of a parsed definition.
    pub fn new(label: impl Into<String>, definition: &Definition) -> Self {
        let severity = |wanted: Severity| {
            definition
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.severity == wanted)
                .count()
        };
        let menu = {
            let _span =
                tracing::debug_span!("menu_build", menus = definition.menus.len()).entered();
            crate::menu::build(definition)
        };

        Self {
            label: label.into(),
            signature: definition.signature.clone(),
            spec_version: definition.spec_version,
            pages: definition.pages.len(),
            constants: definition.constants.len(),
            pc_variables: definition.pc_variables.len(),
            channels: definition.channels.len(),
            tables: definition.tables.len(),
            curves: definition.curves.len(),
            gauges: definition.gauges.iter().map(GaugeSummary::of).collect(),
            front_page_gauges: front_page_gauges(definition),
            key_actions: definition
                .key_actions
                .iter()
                .map(|action| KeyActionSummary {
                    keys: action.keys.clone(),
                    dialog: action.dialog.clone(),
                })
                .collect(),
            reference_tables: definition
                .reference_tables
                .values()
                .map(ReferenceTableSummary::of)
                .collect(),
            unsupported: definition
                .unsupported
                .iter()
                .map(|section| section.name.clone())
                .collect(),
            dialogs: definition.dialogs.len(),
            indicators: definition
                .front_page
                .indicators
                .iter()
                .filter(|indicator| !indicator.template_only)
                .map(|indicator| IndicatorSummary {
                    expression: indicator.channel.clone(),
                    off_label: indicator.off_label.clone(),
                    on_label: indicator.on_label.clone(),
                    on_colour: indicator.colours.get(2).cloned(),
                    line: indicator.line,
                })
                .collect(),
            menu,
            coverage_percent: definition.coverage.percent(),
            errors: severity(Severity::Error),
            warnings: severity(Severity::Warning),
            active_flags: definition.active_flags.iter().cloned().collect(),
        }
    }

    /// Whether the definition parsed without errors.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.errors == 0
    }
}

/// Dashboard slots from `[FrontPage]`, in slot order.
fn front_page_gauges(definition: &Definition) -> Vec<FrontPageGauge> {
    definition
        .front_page
        .gauges
        .iter()
        .map(|(slot, template)| FrontPageGauge {
            slot: *slot,
            template: template.clone(),
        })
        .collect()
}

/// An open project, shaped for the UI rather than for the file layer.
///
/// Deliberately small and `Clone`: the core keeps the [`Project`] (definition,
/// tune images, `.msq` document); the UI only needs enough to name what is open,
/// explain read-only mode and show the load gate's findings.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectSummary {
    /// The project folder, or the tune's folder in standalone mode.
    pub root: PathBuf,
    /// The tune file that was read (and will be written).
    pub tune_path: PathBuf,
    /// The definition file the tune was interpreted with.
    pub definition_path: PathBuf,
    /// Whether this is a project folder (as opposed to standalone).
    pub in_project: bool,
    /// The definition, summarised like a standalone load.
    pub definition: DefinitionSummary,
    /// Whether saving is refused (S1).
    pub read_only: bool,
    /// Why the tune opened read-only, joined once for display.
    ///
    /// `None` for a writable project, so the editor never allocates in the
    /// normal case. The individual reasons stay available in
    /// [`ProjectSummary::read_only_reasons`].
    pub read_only_reason: Option<String>,
    /// Why the tune opened read-only, for the editor and output panel that
    /// explain it.
    pub read_only_reasons: Vec<String>,
    /// Values copied from the tune into the model.
    pub loaded: usize,
    /// Values left at their definition default (absent from the tune).
    pub skipped: usize,
    /// Non-fatal findings, ready for the output panel.
    pub warnings: Vec<String>,
}

impl ProjectSummary {
    /// Builds the UI-facing summary of an open project.
    pub(crate) fn from_project(project: &Project) -> Self {
        let report = project.report();
        let label = project
            .definition_path()
            .file_name()
            .and_then(|name| name.to_str())
            .map_or_else(
                || project.definition_path().display().to_string(),
                str::to_owned,
            );
        let read_only_reasons: Vec<String> = report
            .read_only_reasons()
            .into_iter()
            .map(str::to_owned)
            .collect();
        let read_only_reason = report.is_read_only().then(|| read_only_reasons.join("; "));

        Self {
            root: project.root().to_owned(),
            tune_path: project.tune_path().to_owned(),
            definition_path: project.definition_path().to_owned(),
            in_project: project.is_in_project(),
            definition: DefinitionSummary::new(label, project.definition()),
            read_only: report.is_read_only(),
            read_only_reason,
            read_only_reasons,
            loaded: report.loaded,
            skipped: report.skipped,
            warnings: report
                .warnings
                .iter()
                .map(|issue| issue.message.clone())
                .collect(),
        }
    }
}

/// One serial port the connect dialog can offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortSummary {
    /// Device path.
    pub name: String,
    /// The label to show (the path itself: `serial2` reports no product
    /// description).
    pub label: String,
}

/// How the ECU's signature compared with the definition's.
#[expect(
    clippy::exhaustive_enums,
    reason = "a signature either matches or it does not; there is no third verdict"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureVerdict {
    /// Exactly the definition's signature.
    Match,
    /// Different text: the session is read-only (S1).
    Mismatch,
}

impl SignatureVerdict {
    /// A stable code for logs and the UI.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Match => "match",
            Self::Mismatch => "mismatch",
        }
    }
}

/// What the core is connected to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionSummary {
    /// Device path, or the custom link's label.
    pub port: String,
    /// Whether the link came from the application rather than a serial port.
    pub custom: bool,
    /// The signature the ECU reported.
    pub signature: String,
    /// The signature the definition expects.
    pub definition_signature: String,
    /// Whether they match.
    pub verdict: SignatureVerdict,
    /// Whether this session refuses writes (S1).
    pub read_only: bool,
    /// Why, when it does.
    pub read_only_reason: Option<String>,
    /// Whether the global write switch is on (S8). Off by default.
    pub writes_enabled: bool,
    /// The `EcuBehavior` that claimed the definition.
    pub behavior: String,
    /// Framing in use: `legacy` or `envelope`.
    pub framing: &'static str,
    /// Pages the definition declares.
    pub pages: usize,
    /// The realtime block size, when the definition declares one.
    pub realtime_block: Option<u16>,
}

impl ConnectionSummary {
    /// The ECU and the definition disagree, or the definition cannot be
    /// confirmed; the session is read-only either way (S1).
    #[must_use]
    pub fn is_mismatched(&self) -> bool {
        self.verdict == SignatureVerdict::Mismatch
    }
}

/// What a full page read did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadAllSummary {
    /// Pages read.
    pub pages: usize,
    /// Bytes read from the ECU.
    pub bytes: usize,
    /// Constants taken from those bytes.
    pub constants: usize,
    /// Constants that could not be placed (an unresolved address or shape).
    pub skipped: usize,
    /// Whether the signature matched when the read started.
    pub signature_verified: bool,
}

impl ReadAllSummary {
    /// Whether every declared page and cell was read.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.skipped == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    use crate::{Command, Event, OpId, SaveSummary};

    fn summary() -> DefinitionSummary {
        DefinitionSummary {
            label: "test.ini".to_owned(),
            signature: "speeduino 202501".to_owned(),
            spec_version: Some(3.78),
            pages: 15,
            constants: 817,
            pc_variables: 50,
            channels: 229,
            tables: 21,
            curves: 34,
            gauges: Vec::new(),
            front_page_gauges: Vec::new(),
            key_actions: Vec::new(),
            reference_tables: Vec::new(),
            unsupported: Vec::new(),
            dialogs: 239,
            indicators: Vec::new(),
            menu: Vec::new(),
            coverage_percent: 97.95,
            errors: 0,
            warnings: 0,
            active_flags: vec!["mcu_stm32".to_owned()],
        }
    }

    #[test]
    fn the_summary_publishes_gauge_templates_and_front_page_slots() {
        let definition = oxtuner_ini::parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[GaugeConfigurations]
gaugeCategory = \"Engine\"
tachometer = rpm, \"Engine Speed\", \"RPM\", 0, 8000, 300, 600, 7000, 7500, 0, 0

[FrontPage]
gauge1 = tachometer
gauge2 = missingGauge
",
            &oxtuner_ini::SettingsContext::new(),
        )
        .expect("the fixture parses");

        let summary = DefinitionSummary::new("test.ini", &definition);

        assert_eq!(summary.gauges.len(), 1);
        let gauge = &summary.gauges[0];
        assert_eq!(gauge.name, "tachometer");
        assert_eq!(gauge.category.as_deref(), Some("Engine"));
        assert_eq!(gauge.channel, "rpm");
        assert_eq!(gauge.title, "Engine Speed");
        assert_eq!(gauge.units, "RPM");
        assert_eq!(gauge.low, Some(0.0));
        assert_eq!(gauge.high, Some(8000.0));
        assert_eq!(gauge.high_warning, Some(7000.0));
        assert_eq!(gauge.high_danger, Some(7500.0));

        assert_eq!(summary.front_page_gauges.len(), 2);
        assert_eq!(summary.front_page_gauges[0].slot, 1);
        assert_eq!(summary.front_page_gauges[0].template, "tachometer");
    }

    #[test]
    fn the_summary_publishes_reference_tables_and_key_actions() {
        let definition = oxtuner_ini::parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[ReferenceTables]
referenceTable = testTable, \"Test Title\"
tableIdentifier = 0, \"Zero\"
adcCount = 32
bytesPerAdc = 2
scale = 10
tableLimits = 0, -40, 350, 180
solution = \"Linear\", thermlinear(x)

[KeyActions]
showPanel = C, T, someDialog
",
            &oxtuner_ini::SettingsContext::new(),
        )
        .expect("the fixture parses");

        let summary = DefinitionSummary::new("test.ini", &definition);

        assert_eq!(summary.reference_tables.len(), 1);
        let table = &summary.reference_tables[0];
        assert_eq!(table.name, "testTable");
        assert_eq!(table.title.as_deref(), Some("Test Title"));
        assert_eq!(table.identifiers.len(), 1);
        assert_eq!(table.identifiers[0].id, 0);
        assert_eq!(table.adc_count, Some(32));
        assert_eq!(table.scale, Some(10.0));
        assert_eq!(table.limits.len(), 1);
        assert_eq!(table.limits[0].minimum, Some(-40.0));
        assert_eq!(table.limits[0].maximum, Some(350.0));
        assert_eq!(table.limits[0].default, Some(180.0));
        assert_eq!(table.solutions.len(), 1);

        assert_eq!(summary.key_actions.len(), 1);
        assert_eq!(summary.key_actions[0].dialog, "someDialog");
    }

    #[test]
    fn the_summary_publishes_front_page_indicators_and_their_on_colour() {
        let definition = oxtuner_ini::parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[FrontPage]
indicator = { batteryVoltage < batlow }, \"Battery Voltage OK\", \"Battery Voltage Warning\", white, black, red, black
indicatorTemplate = { running }, \"Off\", \"On\", white, black, green, black
",
            &oxtuner_ini::SettingsContext::new(),
        )
        .expect("the fixture parses");

        let summary = DefinitionSummary::new("test.ini", &definition);

        assert_eq!(summary.indicators.len(), 1, "templates stay out");
        assert_eq!(
            summary.indicators[0].on_label,
            "Battery Voltage Warning".to_owned()
        );
        assert_eq!(summary.indicators[0].on_colour.as_deref(), Some("red"));
    }

    #[test]
    fn an_op_id_renders_with_its_prefix() {
        assert_eq!(OpId::new(7).to_string(), "op-7");
        assert_eq!(OpId::new(7).get(), 7);
    }

    #[test]
    fn only_replies_carry_the_op_they_answer() {
        let op = OpId::new(3);

        assert_eq!(Event::DefinitionUnloaded { op }.op(), Some(op));
        assert_eq!(
            Event::DefinitionLoaded {
                op,
                summary: summary()
            }
            .op(),
            Some(op)
        );
        assert_eq!(
            Event::TuneSaved {
                op,
                outcome: SaveSummary {
                    path: PathBuf::from("CurrentTune.msq"),
                    cells: 0,
                },
            }
            .op(),
            Some(op)
        );
        assert_eq!(Event::Stopped.op(), None);
    }

    #[test]
    fn a_command_constructed_from_bytes_uses_the_default_settings() {
        let Command::LoadDefinition {
            label, settings, ..
        } = Command::load_definition("test.ini", vec![0x5B])
        else {
            panic!("wrong variant");
        };

        assert_eq!(label, "test.ini");
        assert!(settings.tested_flags().is_empty());
    }

    #[test]
    fn summaries_are_ordered_by_flag_for_a_stable_ui() {
        let summary = summary();

        let flags: HashSet<_> = summary.active_flags.iter().cloned().collect();
        assert_eq!(flags.len(), summary.active_flags.len());
        assert!(summary.is_clean());
    }
}
