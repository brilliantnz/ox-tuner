//! One dialog, shaped for the constant list.

use std::sync::Arc;

use crate::contract::GateState;

/// What a dialog row holds, for the UI to pick a presentation.
#[expect(
    clippy::exhaustive_enums,
    reason = "a dialog row is a value, a select, a slider, a toggle or a note"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogFieldKind {
    /// A scalar value from `field`, `displayOnlyField` or a setting.
    Value,
    /// A `bits` value with options, drawn as a drop-down.
    Select,
    /// A scalar from `slider`, drawn as a slider.
    Slider,
    /// A boolean from `checkbox` or `radio`; the value is 0 or 1.
    Toggle,
    /// Text with no variable: a hint or a warning the definition shows.
    Note,
}

/// The colour a definition gives a row label.
///
/// A `!` prefix means red (danger) and `#` means blue (info), per the dialect.
#[expect(
    clippy::exhaustive_enums,
    reason = "the dialect defines normal, red and blue labels"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogFieldTone {
    /// The definition gives the label no colour.
    Normal,
    /// A `!` prefix.
    Danger,
    /// A `#` prefix.
    Info,
}

/// One option of a `bits` constant, shaped for a select control.
///
/// Values are the raw option numbers from the definition. The UI shows the
/// label and writes the value. Entries the dialect marks `INVALID` never reach
/// this list: a 65 536-entry CAN address table would otherwise copy tens of
/// megabytes into every dialog snapshot, and the picker hides them anyway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogSelectOption {
    /// Raw option value.
    pub value: u32,
    /// Label from the definition.
    pub label: String,
}

/// One row of a dialog, shaped for the constant list.
///
/// Values are in **user units**. The UI keeps no tune values of its own: it
/// renders this snapshot and asks for edits by variable.
#[expect(
    clippy::struct_excessive_bools,
    reason = "changed, display-only, writable and hex are independent row facts"
)]
#[derive(Debug, Clone, PartialEq)]
pub struct DialogField {
    /// Label from the row, unquoted.
    pub label: String,
    /// Tune variable the row writes, empty for a note.
    pub variable: String,
    /// User value, for scalar numbers.
    pub value: Option<f64>,
    /// Text, for string variables and notes.
    pub text: Option<String>,
    /// Units of the value.
    pub units: String,
    /// Decimal digits to display.
    pub digits: u8,
    /// Whether the row differs from the file baseline.
    pub changed: bool,
    /// Whether the definition marks the row display-only.
    pub display_only: bool,
    /// Whether the session may store an edit to this row.
    pub writable: bool,
    /// What the row holds, so a toggle is not drawn as a plain number.
    pub kind: DialogFieldKind,
    /// The nested panel title the row came from, empty at the dialog's top.
    pub section: String,
    /// Colour the definition gives the label (`!` or `#` before the text).
    pub tone: DialogFieldTone,
    /// Select options from the definition's `bits` list, empty for numbers.
    ///
    /// Shared between every row that names the same variable: one `Arc` per
    /// walk, so a CAN dialog with nineteen 65 536-entry address lists keeps a
    /// single filtered copy.
    pub options: Arc<[DialogSelectOption]>,
    /// Visibility condition from the row (`{ … }`), as written.
    pub visibility_expression: Option<String>,
    /// Enable condition from the row (`{ … }`), as written.
    pub enable_expression: Option<String>,
    /// Whether the definition's gates let this row appear and accept edits.
    pub gates: GateState,
    /// Help text for this variable from `[SettingContextHelp]`.
    pub help: Option<String>,
    /// Lowest user value the definition accepts, when stated.
    pub minimum: Option<f64>,
    /// Highest user value the definition accepts, when stated.
    pub maximum: Option<f64>,
    /// Array element this row edits (`var[2]`), none for whole variables.
    pub index: Option<usize>,
    /// Whether the row asks for hexadecimal display (`displayInHex`).
    pub hex: bool,
    /// Whether a checkbox shows its checked state on raw zero (`flipValue`).
    pub flip: bool,
    /// Whether a change to this row needs an ECU power cycle.
    pub power_cycle: bool,
}

/// One dialog, shaped for the constant list.
///
/// Panels are flattened into the field list: a dialog like `Engine Constants`
/// is a border layout of panels, and its constants live one or two levels down.
/// Tables and curves a panel embeds stay out; the menu opens them as grids.
#[derive(Debug, Clone, PartialEq)]
pub struct DialogView {
    /// The definition's dialog name.
    pub id: String,
    /// Window title from the definition, or the dialog name.
    pub title: String,
    /// Layout from the definition (`yAxis`, `border`, …), when stated.
    pub layout: Option<String>,
    /// Rows in document order, panels flattened in place.
    pub fields: Vec<DialogField>,
    /// Number of fields that differ from the file baseline.
    pub changed: usize,
    /// Whether the session may store edits.
    pub writable: bool,
    /// Built-in `std_*` panels this dialog embeds, which this build cannot draw.
    pub unavailable: Vec<String>,
    /// Dialog-level help URL from `topicHelp`, when the definition states one.
    pub topic_help: Option<String>,
    /// Help links from `help = topic, "Title"` and `webHelp` rows.
    pub help_links: Vec<DialogHelpLink>,
    /// Sensor presets from `settingSelector` groups and their options.
    pub presets: Vec<DialogPreset>,
    /// Action buttons from `commandButton` rows.
    pub actions: Vec<DialogAction>,
    /// Embedded gauges, indicators, graphs and readouts, as data.
    ///
    /// The core does not draw these yet; carrying them keeps a dialog that is
    /// mostly indicators (common in rusEFI definitions) from reading as empty.
    pub widgets: Vec<DialogWidget>,
    /// Tables a panel embeds, in document order, with their panel gates.
    ///
    /// The grid opens these; they stay out of [`Self::fields`] because a grid
    /// is not a row.
    pub tables: Vec<DialogEmbed>,
    /// Curves a panel embeds, in document order, with their panel gates.
    pub curves: Vec<DialogEmbed>,
    /// Rows no summary covers yet, in document order.
    pub unmodeled: Vec<DialogUnmodeledRow>,
}

/// One table or curve a dialog panel embeds, with the gates it carries.
///
/// A `panel = veTable {enable}, {visible}` row keeps its condition here: the
/// expressions stay raw and the state resolved, the same shape every other
/// gated row uses, so the grid can respect them when the editor opens it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogEmbed {
    /// Table id or curve name, as the definition spells it.
    pub id: String,
    /// The nested panel title the row came from, empty at the dialog's top.
    pub section: String,
    /// Visibility condition from the row (`{ … }`), as written.
    pub visibility_expression: Option<String>,
    /// Enable condition from the row (`{ … }`), as written.
    pub enable_expression: Option<String>,
    /// Whether the definition's gates let this embed appear and be edited.
    pub gates: GateState,
}

/// One dialog row no summary covers yet, kept as data.
///
/// New or exotic keywords (`userPassword`, future selectors) land here instead
/// of vanishing: the parser already keeps them, and the summary says how many
/// there are and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogUnmodeledRow {
    /// Row keyword as written (`userPassword`, `array1D`, …).
    pub keyword: String,
    /// Fields after the keyword, as written.
    pub fields: Vec<String>,
}

/// One help link from a `help = topic, "Title"` or `webHelp` row.
///
/// A two-field `help` row is a reference, not text: the first field names a
/// help topic (a menu target like `helpGeneral`, or a `topicHelp` id), the
/// second is the title shown. A `webHelp` row carries a URL as its topic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogHelpLink {
    /// Help topic id or URL, as written.
    pub topic: String,
    /// Title from the row, empty for a bare `webHelp` URL.
    pub title: String,
}

/// One sensor preset group from a `settingSelector` and its options.
///
/// A preset writes several constants at once (`mapMin=10, mapMax=121`), so it
/// cannot be one value row. The assignments stay as written; evaluating them
/// belongs to a future edit path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogPreset {
    /// Label from the selector row.
    pub label: String,
    /// Visibility condition from the selector (`{ … }`), as written.
    pub visibility_expression: Option<String>,
    /// Enable condition from the selector (`{ … }`), as written.
    pub enable_expression: Option<String>,
    /// Whether the definition's gates let this selector appear and accept picks.
    pub gates: GateState,
    /// Options in document order.
    pub options: Vec<DialogPresetOption>,
}

/// One option of a [`DialogPreset`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogPresetOption {
    /// Label from the option row.
    pub label: String,
    /// Constant assignments from the row, in document order.
    pub assignments: Vec<DialogPresetAssignment>,
    /// Whether any assignment changes a constant that needs a power cycle.
    pub power_cycle: bool,
}

/// One `variable=value` assignment of a [`DialogPresetOption`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogPresetAssignment {
    /// Constant the option writes.
    pub variable: String,
    /// Value as written (a number or a label).
    pub value: String,
    /// Whether a change to this constant needs a power cycle.
    pub power_cycle: bool,
}

/// One action button from a `commandButton` row.
///
/// The command runs through `[ControllerCommands]`; `known` tells whether the
/// definition declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogAction {
    /// Label from the row.
    pub label: String,
    /// Command the button sends.
    pub command: String,
    /// Visibility condition from the row (`{ … }`), as written.
    pub visibility_expression: Option<String>,
    /// Enable condition from the row (`{ … }`), as written.
    pub enable_expression: Option<String>,
    /// Whether the definition's gates let this button appear and be pressed.
    pub gates: GateState,
    /// Whether `[ControllerCommands]` declares the command.
    pub known: bool,
    /// Bare flags from the row (`closeDialogOnClick`, `showMessageOnClick` and
    /// its message, …), in document order.
    pub flags: Vec<String>,
}

/// What an embedded widget row holds, for the UI to pick a presentation.
#[expect(
    clippy::exhaustive_enums,
    reason = "a widget row is one of the definition's fixed row kinds"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogWidgetKind {
    /// A `gauge` template reference.
    Gauge,
    /// An `indicator` expression with off/on labels.
    Indicator,
    /// A `liveGraph` channel trace.
    LiveGraph,
    /// A `graphLine` channel within a graph.
    GraphLine,
    /// A `readout` value or gauge reference.
    Readout,
    /// A `runtimeValue` live channel reading.
    RuntimeValue,
}

/// One embedded gauge, indicator, graph or readout, as data.
#[derive(Debug, Clone, PartialEq)]
pub struct DialogWidget {
    /// What the row holds, so a graph line is not drawn as a gauge.
    pub kind: DialogWidgetKind,
    /// Label from the row, or the target when the row names none.
    pub label: String,
    /// What it shows: a gauge name, an indicator expression, or a channel.
    pub target: String,
    /// Visibility condition from the row (`{ … }`), as written.
    pub visibility_expression: Option<String>,
    /// Enable condition from the row (`{ … }`), as written.
    pub enable_expression: Option<String>,
    /// Whether the definition's gates let this widget appear and update.
    pub gates: GateState,
    /// The nested panel title the row came from, empty at the dialog's top.
    pub section: String,
    /// Display units from the row, empty when the gauge template provides them.
    pub units: String,
    /// Display minimum from the row, when the row states a number.
    pub minimum: Option<f64>,
    /// Display maximum from the row, when the row states a number.
    pub maximum: Option<f64>,
}
