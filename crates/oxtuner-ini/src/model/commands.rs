use crate::fields::CommandTemplate;

use super::Number;

/// One raw controller command.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ControllerCommand {
    /// Command template name.
    pub name: String,
    /// Commands executed in order; envelope bytes excluded.
    pub commands: Vec<CommandTemplate>,
    /// Description, when `displayCommand` opted the command into the UI.
    pub description: Option<String>,
    /// Source line.
    pub line: u32,
}

/// One sensor calibration table from `[ReferenceTables]`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Default)]
pub struct ReferenceTable {
    /// Table name.
    pub name: String,
    /// Display title.
    pub title: Option<String>,
    /// Target table identifiers and labels.
    pub identifiers: Vec<(u32, String)>,
    /// Number of translated elements.
    pub adc_count: Option<u32>,
    /// Element width in bytes.
    pub bytes_per_adc: Option<u32>,
    /// Scale applied before writing.
    pub scale: Option<Number>,
    /// Optional start offset.
    pub start_offset: Option<u32>,
    /// Per-identifier limits and defaults.
    pub limits: Vec<(u32, Number, Number, Number)>,
    /// Solution labels and expressions, as written.
    pub solutions: Vec<(String, String)>,
    /// Source line.
    pub line: u32,
}

/// One `[Datalog]` field.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DatalogEntry {
    /// Channel name.
    pub channel: String,
    /// Column label.
    pub label: String,
    /// Declared value type (`float` / `int`), legacy.
    pub value_type: Option<String>,
    /// Format string or boolean tag.
    pub format: String,
    /// Enable condition, as written.
    pub enable: Option<String>,
    /// Record lag.
    pub lag: Option<String>,
    /// Category, from the most recent `category =` line.
    pub category: Option<String>,
    /// Source line.
    pub line: u32,
}

/// One hidden-dialog hotkey from `[KeyActions]`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct KeyAction {
    /// Keys in order.
    pub keys: String,
    /// Dialog the keys open.
    pub dialog: String,
    /// Source line.
    pub line: u32,
}
