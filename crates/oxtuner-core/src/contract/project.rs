//! Project and definition snapshots the UI names what is open with.

use std::path::PathBuf;

use crate::menu::MenuNode;

use super::{FrontPageGauge, GaugeTemplate, IndicatorSpec};

/// A definition the new-project wizard inspected, before anything was created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionPreview {
    /// Firmware signature, trimmed.
    pub signature: String,
    /// Flags the definition asks about in an `#if`, sorted.
    pub tested_flags: Vec<String>,
    /// The setting groups the definition declares, in declaration order.
    pub groups: Vec<SettingGroupView>,
}

impl DefinitionPreview {
    /// The groups the new-project wizard shows.
    ///
    /// The definition's own `[SettingGroups]` come first-class. The display
    /// choices the reference application keeps in its own configuration are
    /// owned by the application too, so a definition never declares them; they
    /// are offered only when it tests the matching flag (`#if LAMBDA`,
    /// `#if CELSIUS`), and only when the definition has no group of its own
    /// for that choice.
    ///
    /// The labels are the canonical English text; the UI localizes them by
    /// group and option name. The rule lives here so the configuration step,
    /// the review and creation cannot drift apart.
    #[must_use]
    pub fn groups_with_display_choices(&self) -> Vec<SettingGroupView> {
        let tests = |flag: &str| {
            self.tested_flags
                .iter()
                .any(|tested| tested.eq_ignore_ascii_case(flag))
        };
        let option = |name: &str, label: &str| SettingOptionView {
            name: name.to_owned(),
            label: label.to_owned(),
        };

        // A definition can declare these choices itself (`lambdaSensor`, a
        // `CELSIUS` option, …). Its labels win: the application row exists
        // only when the file has no such group.
        let declared = |name: &str, option_name: &str| {
            self.groups.iter().any(|group| {
                group.name.eq_ignore_ascii_case(name)
                    || group
                        .options
                        .iter()
                        .any(|option| option.name.eq_ignore_ascii_case(option_name))
            })
        };

        let mut groups = Vec::new();
        if tests("lambda") && !declared("lambdaSensor", "LAMBDA") {
            groups.push(SettingGroupView {
                name: "lambda_display".to_owned(),
                label: "Lambda display".to_owned(),
                options: vec![option("DEFAULT", "AFR"), option("LAMBDA", "Lambda")],
                default: Some("DEFAULT".to_owned()),
                default_active: false,
            });
        }
        if tests("celsius") && !declared("temperature", "CELSIUS") {
            groups.push(SettingGroupView {
                name: "temperature_display".to_owned(),
                label: "Temperature display".to_owned(),
                options: vec![
                    option("DEFAULT", "Fahrenheit"),
                    option("CELSIUS", "Celsius"),
                ],
                default: Some("DEFAULT".to_owned()),
                default_active: false,
            });
        }

        groups.extend(self.groups.iter().cloned());
        groups
    }
}

/// One `[SettingGroups]` entry as the wizard shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingGroupView {
    /// Group id the `.msq` snapshot records.
    pub name: String,
    /// Display name.
    pub label: String,
    /// Options in declaration order. Empty for a boolean group.
    pub options: Vec<SettingOptionView>,
    /// The option selected when the user makes no choice. `None` for a
    /// boolean group, whose default is [`Self::default_active`].
    pub default: Option<String>,
    /// Whether a boolean group's flag is active by default.
    pub default_active: bool,
}

/// One option of a [`SettingGroupView`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingOptionView {
    /// Flag name the definition's `#if` tests, and what creation stores.
    pub name: String,
    /// Display name.
    pub label: String,
}

impl DefinitionPreview {
    pub(crate) fn of(preview: &oxtuner_project::DefinitionPreview) -> Self {
        Self {
            signature: preview.signature.clone(),
            tested_flags: preview.tested_flags.clone(),
            groups: preview
                .groups
                .iter()
                .map(|group| SettingGroupView {
                    name: group.name.clone(),
                    label: group.label.clone(),
                    options: group
                        .options
                        .iter()
                        .map(|option| SettingOptionView {
                            name: option.name.clone(),
                            label: option.label.clone(),
                        })
                        .collect(),
                    default: group.default.clone(),
                    default_active: group.default_active,
                })
                .collect(),
        }
    }
}

/// One hidden-dialog hotkey from `[KeyActions]`, as the UI lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyActionInfo {
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
pub struct ReferenceTableView {
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

/// One target of a [`ReferenceTableView`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceTableIdentifier {
    /// Target table identifier.
    pub id: u32,
    /// Target label.
    pub label: String,
}

/// One write limit of a [`ReferenceTableView`]: identifier, min, max,
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

/// One solution of a [`ReferenceTableView`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferenceTableSolution {
    /// Solution label.
    pub label: String,
    /// Solution expression, as written.
    pub expression: String,
}

#[derive(Debug, Clone, PartialEq)]
/// Deliberately small and `Clone`: the core keeps the full [`oxtuner_ini::Definition`];
/// the UI only needs enough to name what it is looking at.
pub struct DefinitionView {
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
    pub gauges: Vec<GaugeTemplate>,
    /// Dashboard slots from `[FrontPage]`, in slot order.
    pub front_page_gauges: Vec<FrontPageGauge>,
    /// Hidden-dialog hotkeys from `[KeyActions]`, in definition order.
    pub key_actions: Vec<KeyActionInfo>,
    /// Sensor calibration tables from `[ReferenceTables]`, in definition order.
    pub reference_tables: Vec<ReferenceTableView>,
    /// Sections kept as raw data, by name.
    pub unsupported: Vec<String>,
    /// Dialogs.
    pub dialogs: usize,
    /// `[FrontPage]` indicators, in definition order.
    pub indicators: Vec<IndicatorSpec>,
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

#[derive(Debug, Clone, PartialEq)]
/// Deliberately small and `Clone`: the core keeps the project;
/// the UI only needs enough to name what is open, explain read-only mode and
/// show the load gate findings.
pub struct ProjectView {
    /// The project folder, or the tune's folder in standalone mode.
    pub root: PathBuf,
    /// The tune file that was read (and will be written).
    pub tune_path: PathBuf,
    /// The definition file the tune was interpreted with.
    pub definition_path: PathBuf,
    /// Whether this is a project folder (as opposed to standalone).
    pub in_project: bool,
    /// The definition, summarised like a standalone load.
    pub definition: DefinitionView,
    /// Whether saving is refused.
    pub read_only: bool,
    /// Why the tune opened read-only, joined once for display.
    ///
    /// `None` for a writable project, so the editor never allocates in the
    /// normal case. The individual reasons stay available in
    /// [`ProjectView::read_only_reasons`].
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
    /// The panel layout this project was last read with.
    pub layout: oxtuner_project::ProjectLayout,
    /// The connection this project last used, when state has it.
    pub connection: oxtuner_project::ConnectionState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// One serial port present on the machine.
///
/// Every field takes part in the row comparison, so a different device on the
/// same path reads as a change.
pub struct PortView {
    /// Device path.
    pub name: String,
    /// The label to show: a known board name, else the product string, else the
    /// path.
    pub label: String,
    /// Vendor id, when the platform reports one.
    ///
    /// Kept so a different device arriving on the same path counts as a change
    /// rather than as the same port.
    pub vid: Option<u16>,
    /// Product id, when the platform reports one.
    pub pid: Option<u16>,
    /// Serial number, when the device provides one.
    pub serial: Option<String>,
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
    /// Different text: the session is read-only.
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
pub struct ConnectionView {
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
    /// Whether this session refuses writes.
    pub read_only: bool,
    /// Why, when it does.
    pub read_only_reason: Option<String>,
    /// Whether the global write switch is on. Off by default.
    pub writes_enabled: bool,
    /// The stable [`EcuBehavior::id`] that claimed the definition, never a
    /// display label.
    ///
    /// [`EcuBehavior::id`]: oxtuner_protocol::EcuBehavior::id
    pub behavior: String,
    /// Framing in use: `legacy` or `envelope`.
    pub framing: &'static str,
    /// Pages the definition declares.
    pub pages: usize,
    /// The realtime block size, when the definition declares one.
    pub realtime_block: Option<u16>,
}

/// The outcome of a signature test against one port.
///
/// A test proves a port, a baud and a framing without opening a session, so it
/// is also what a new project checks a definition against.
///
/// The port is kept beside the summary on purpose: on failure the summary is
/// gone and the port is the only thing the dialog can still show. On success
/// it always equals the summary's own port. The baud is kept for the same
/// reason: a result only describes the rate it ran at, so a dialog that
/// changed the picker afterwards must not keep showing it.
#[derive(Debug, Clone, PartialEq)]
pub struct ConnectionTest {
    /// The port the test ran against.
    pub port: String,
    /// The rate it ran at, when the choice named one.
    pub baud: Option<u32>,
    /// The summary, or the failure message.
    pub outcome: Result<ConnectionView, String>,
}

impl ConnectionTest {
    /// The signature a create should verify, when this test ran on `port`.
    ///
    /// A test against another port says nothing about the ECU a project will
    /// talk to, so its signature is ignored rather than carried over.
    #[must_use]
    pub fn signature_for(&self, port: Option<&str>) -> Option<String> {
        if port != Some(self.port.as_str()) {
            return None;
        }

        match &self.outcome {
            Ok(view) => Some(view.signature.trim().to_owned()),
            Err(_) => None,
        }
    }
}

/// One cell the ECU and the working image disagree about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConflictReport {
    /// Variable name.
    pub name: String,
    /// Table row, or zero for a scalar.
    pub row: usize,
    /// Table column, or zero for a scalar.
    pub column: usize,
    /// The value the user is working with.
    pub working: String,
    /// The value the ECU read back.
    pub ecu: String,
}

/// What a full page read did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadReport {
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
    /// Cells that differ from the working image after a re-read.
    pub conflicts: Vec<ConflictReport>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tested(port: &str, signature: &str) -> ConnectionTest {
        ConnectionTest {
            port: port.to_owned(),
            baud: Some(115_200),
            outcome: Ok(ConnectionView {
                port: port.to_owned(),
                custom: false,
                signature: signature.to_owned(),
                definition_signature: signature.to_owned(),
                verdict: SignatureVerdict::Match,
                read_only: false,
                read_only_reason: None,
                writes_enabled: false,
                behavior: "test".to_owned(),
                framing: "legacy",
                pages: 1,
                realtime_block: None,
            }),
        }
    }

    #[test]
    fn a_signature_counts_only_when_the_check_ran_on_this_port() {
        let result = tested("/dev/ttyACM0", "speeduino 202501");

        assert_eq!(
            result.signature_for(Some("/dev/ttyACM0")).as_deref(),
            Some("speeduino 202501")
        );
        assert_eq!(result.signature_for(Some("/dev/ttyACM1")), None);
        assert_eq!(result.signature_for(None), None);
    }

    #[test]
    fn a_failed_check_contributes_no_signature() {
        let failed = ConnectionTest {
            port: "/dev/ttyACM0".to_owned(),
            baud: Some(115_200),
            outcome: Err("no answer".to_owned()),
        };

        assert_eq!(failed.signature_for(Some("/dev/ttyACM0")), None);
    }
}
