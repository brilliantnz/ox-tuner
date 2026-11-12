//! Project and definition snapshots the UI names what is open with.

use std::path::PathBuf;

use crate::menu::MenuNode;

use super::{FrontPageGauge, GaugeTemplate, IndicatorSpec};

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
    pub ui: oxtuner_project::UiState,
    /// The connection this project last used, when state has it.
    pub connection: ProjectConnection,
}

/// The project's remembered connection: what the connect dialog starts from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectConnection {
    /// Serial port last opened from this project.
    pub last_port: Option<String>,
    /// Baud rate paired with that port.
    pub baud_override: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// One serial port the connect dialog can offer.
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
