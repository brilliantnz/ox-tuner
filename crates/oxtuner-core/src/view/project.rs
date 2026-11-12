use oxtuner_ini::{Definition, Number, Severity};
use oxtuner_project::Project;

use crate::contract::frontpage::{FrontPageGauge, GaugeTemplate, IndicatorSpec};
use crate::contract::project::{
    ConnectionView, DefinitionView, ProjectConnection, ProjectView, ReadReport,
    ReferenceTableIdentifier, ReferenceTableLimit, ReferenceTableSolution, ReferenceTableView,
};
use crate::contract::{KeyActionInfo, SignatureVerdict};
use crate::gate::GateEvaluator;
use crate::realtime::RealtimeFrame;

impl ReferenceTableView {
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

impl DefinitionView {
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
            gauges: definition.gauges.iter().map(GaugeTemplate::of).collect(),
            front_page_gauges: front_page_gauges(definition),
            key_actions: definition
                .key_actions
                .iter()
                .map(|action| KeyActionInfo {
                    keys: action.keys.clone(),
                    dialog: action.dialog.clone(),
                })
                .collect(),
            reference_tables: definition
                .reference_tables
                .values()
                .map(ReferenceTableView::of)
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
                .map(|indicator| IndicatorSpec {
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

impl ProjectView {
    /// Builds the UI-facing summary of an open project.
    ///
    /// The menu is evaluated here because its gates depend on the tune. A
    /// definition loaded without a tune keeps the raw menu and leaves every
    /// node open.
    pub(crate) fn from_project(project: &Project, live: Option<&RealtimeFrame>) -> Self {
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

        let mut definition = DefinitionView::new(label, project.definition());
        let mut evaluator = GateEvaluator::new(project.definition(), project.tune(), live);
        crate::menu::evaluate(&mut definition.menu, &mut evaluator);

        let unresolved: Vec<&str> = evaluator.unresolved().collect();
        if !unresolved.is_empty() {
            tracing::debug!(
                unresolved = ?unresolved,
                "menu gates could not be evaluated"
            );
        }

        Self {
            root: project.root().to_owned(),
            tune_path: project.tune_path().to_owned(),
            definition_path: project.definition_path().to_owned(),
            in_project: project.is_in_project(),
            definition,
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
            connection: ProjectConnection {
                last_port: project.state().connection.last_port.clone(),
                baud_override: project.state().connection.baud_override,
            },
            ui: project.state().ui.clone(),
        }
    }
}

impl ConnectionView {
    /// The ECU and the definition disagree, or the definition cannot be
    /// confirmed; the session is read-only either way.
    #[must_use]
    pub fn is_mismatched(&self) -> bool {
        self.verdict == SignatureVerdict::Mismatch
    }
}

impl ReadReport {
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

    use std::path::PathBuf;

    use crate::contract::CommandRequest;
    use crate::{Event, OpId, SaveReport};

    fn summary() -> DefinitionView {
        DefinitionView {
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

        let summary = DefinitionView::new("test.ini", &definition);

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

        let summary = DefinitionView::new("test.ini", &definition);

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

        let summary = DefinitionView::new("test.ini", &definition);

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
                view: summary()
            }
            .op(),
            Some(op)
        );
        assert_eq!(
            Event::TuneSaved {
                op,
                outcome: SaveReport {
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
        let CommandRequest::LoadDefinition {
            label, settings, ..
        } = CommandRequest::load_definition("test.ini", vec![0x5B])
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
