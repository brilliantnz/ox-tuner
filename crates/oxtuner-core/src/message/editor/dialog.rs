//! One dialog, shaped for the constant list.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use oxtuner_ini::{DialogRow, RowKind};
use oxtuner_project::Project;

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
    reason = "changed, display-only, writable, warning and hex are independent row facts"
)]
#[derive(Debug, Clone, PartialEq)]
pub struct DialogFieldSummary {
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
    /// Whether a note is a warning (`!` or `#` before the text).
    pub warning: bool,
    /// Select options from the definition's `bits` list, empty for numbers.
    ///
    /// Shared between every row that names the same variable: one `Arc` per
    /// walk, so a CAN dialog with nineteen 65 536-entry address lists keeps a
    /// single filtered copy.
    pub options: Arc<[DialogSelectOption]>,
    /// Visibility condition from the row (`{ … }`), as written.
    pub visibility: Option<String>,
    /// Enable condition from the row (`{ … }`), as written.
    pub enabled: Option<String>,
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
}

/// One dialog, shaped for the constant list.
///
/// Panels are flattened into the field list: a dialog like `Engine Constants`
/// is a border layout of panels, and its constants live one or two levels down.
/// Tables and curves a panel embeds stay out; the menu opens them as grids.
#[derive(Debug, Clone, PartialEq)]
pub struct DialogSummary {
    /// The definition's dialog name.
    pub id: String,
    /// Window title from the definition, or the dialog name.
    pub title: String,
    /// Layout from the definition (`yAxis`, `border`, …), when stated.
    pub layout: Option<String>,
    /// Rows in document order, panels flattened in place.
    pub fields: Vec<DialogFieldSummary>,
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
    pub commands: Vec<DialogCommand>,
    /// Embedded gauges, indicators, graphs and readouts, as data.
    ///
    /// The core does not draw these yet; carrying them keeps a dialog that is
    /// mostly indicators (common in rusEFI definitions) from reading as empty.
    pub widgets: Vec<DialogWidget>,
    /// Tables a panel embeds, in document order. The menu opens them as grids.
    pub tables: Vec<String>,
    /// Curves a panel embeds, in document order.
    pub curves: Vec<String>,
    /// Rows no summary covers yet, in document order.
    pub unmodeled: Vec<DialogUnmodeledRow>,
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
    pub visibility: Option<String>,
    /// Enable condition from the selector (`{ … }`), as written.
    pub enabled: Option<String>,
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
}

/// One `variable=value` assignment of a [`DialogPresetOption`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogPresetAssignment {
    /// Constant the option writes.
    pub variable: String,
    /// Value as written (a number or a label).
    pub value: String,
}

/// One action button from a `commandButton` row.
///
/// The command runs through `[ControllerCommands]`; `known` tells whether the
/// definition declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogCommand {
    /// Label from the row.
    pub label: String,
    /// Command the button sends.
    pub command: String,
    /// Visibility condition from the row (`{ … }`), as written.
    pub visibility: Option<String>,
    /// Enable condition from the row (`{ … }`), as written.
    pub enabled: Option<String>,
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
    pub visibility: Option<String>,
    /// Enable condition from the row (`{ … }`), as written.
    pub enabled: Option<String>,
    /// The nested panel title the row came from, empty at the dialog's top.
    pub section: String,
    /// Display units from the row, empty when the gauge template provides them.
    pub units: String,
    /// Display minimum from the row, when the row states a number.
    pub minimum: Option<f64>,
    /// Display maximum from the row, when the row states a number.
    pub maximum: Option<f64>,
}

impl DialogSummary {
    /// Reads one dialog's rows from an open project, panels flattened.
    ///
    /// A dialog is a layout: `Engine Constants` is a border of panels, and the
    /// constants sit one or two levels down. Reading only its own rows would
    /// show an empty list, so panels are walked in place and their rows keep the
    /// nested title as a section. Rows that name no tune variable and carry one
    /// field are the definition's own text, kept as notes. A built-in `std_*`
    /// panel cannot be drawn from the definition and is reported by name.
    ///
    /// Tables and curves a panel embeds are grids, not rows: the menu opens
    /// them, and only the grid's snapshot carries their values.
    pub(crate) fn from_project(project: &Project, name: &str) -> Result<Self, String> {
        let definition = project.definition();
        let dialog = definition
            .dialogs
            .get_ci(name)
            .ok_or_else(|| format!("the definition has no dialog named `{name}`"))?;

        let mut walk = RowWalk::new(project);
        walk.walk(&dialog.name, "");
        let finished = walk.finish();

        let changed = finished.fields.iter().filter(|field| field.changed).count();
        Ok(Self {
            id: dialog.name.clone(),
            title: dialog
                .title
                .clone()
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| dialog.name.clone()),
            fields: finished.fields,
            changed,
            writable: !project.is_read_only(),
            unavailable: finished.unavailable,
            topic_help: finished.topic_help,
            layout: dialog.layout.clone(),
            help_links: finished.help_links,
            presets: finished.presets,
            commands: finished.commands,
            widgets: finished.widgets,
            tables: finished.tables,
            curves: finished.curves,
            unmodeled: finished.unmodeled,
        })
    }
}

/// The collected rows of one dialog walk.
struct FinishedWalk {
    fields: Vec<DialogFieldSummary>,
    unavailable: Vec<String>,
    topic_help: Option<String>,
    help_links: Vec<DialogHelpLink>,
    presets: Vec<DialogPreset>,
    commands: Vec<DialogCommand>,
    widgets: Vec<DialogWidget>,
    tables: Vec<String>,
    curves: Vec<String>,
    unmodeled: Vec<DialogUnmodeledRow>,
}

/// The accumulators of one dialog walk.
///
/// Panels recurse, so every row a dialog flattens into travels together
/// instead of as one long argument list.
struct RowWalk<'project> {
    project: &'project Project,
    visited: HashSet<String>,
    gates: InheritedGates,
    fields: Vec<DialogFieldSummary>,
    unavailable: Vec<String>,
    topic_help: Option<String>,
    help_links: Vec<DialogHelpLink>,
    presets: Vec<DialogPreset>,
    pending_preset: Option<usize>,
    commands: Vec<DialogCommand>,
    widgets: Vec<DialogWidget>,
    tables: Vec<String>,
    curves: Vec<String>,
    unmodeled: Vec<DialogUnmodeledRow>,
    options_cache: HashMap<String, Arc<[DialogSelectOption]>>,
}

impl<'project> RowWalk<'project> {
    fn new(project: &'project Project) -> Self {
        Self {
            project,
            visited: HashSet::new(),
            gates: InheritedGates::default(),
            fields: Vec::new(),
            unavailable: Vec::new(),
            topic_help: None,
            help_links: Vec::new(),
            presets: Vec::new(),
            pending_preset: None,
            commands: Vec::new(),
            widgets: Vec::new(),
            tables: Vec::new(),
            curves: Vec::new(),
            unmodeled: Vec::new(),
            options_cache: HashMap::new(),
        }
    }

    fn finish(self) -> FinishedWalk {
        FinishedWalk {
            fields: self.fields,
            unavailable: self.unavailable,
            topic_help: self.topic_help,
            help_links: self.help_links,
            presets: self.presets,
            commands: self.commands,
            widgets: self.widgets,
            tables: self.tables,
            curves: self.curves,
            unmodeled: self.unmodeled,
        }
    }

    /// Records an embedded table or curve once, in document order.
    fn record_embedded(&mut self, target: &str) {
        let definition = self.project.definition();

        if let Some(table) = definition
            .tables
            .get_ci(target)
            .or_else(|| {
                definition.tables.values().find(|table| {
                    table
                        .map_id
                        .as_deref()
                        .is_some_and(|map| map.eq_ignore_ascii_case(target))
                })
            })
            .map(|table| table.id.clone())
        {
            if !self.tables.iter().any(|id| id == &table) {
                self.tables.push(table);
            }
            return;
        }

        if let Some(curve) = definition
            .curves
            .get_ci(target)
            .map(|curve| curve.name.clone())
            && !self.curves.iter().any(|name| name == &curve)
        {
            self.curves.push(curve);
        }
    }

    /// Select options and help text of one variable, from the definition.
    ///
    /// Bits constants carry their drop-down list here; every other class yields
    /// no options. Entries the dialect marks `INVALID` are skipped, and one
    /// `Arc` per variable is shared by every row that names it. Help comes from
    /// `[SettingContextHelp]`.
    fn presentation(&mut self, variable: &str) -> (Arc<[DialogSelectOption]>, Option<String>) {
        let definition = self.project.definition();

        let options = Arc::clone(
            self.options_cache
                .entry(variable.to_owned())
                .or_insert_with(|| {
                    definition
                        .constant(variable)
                        .map(|constant| constant.options.as_ref())
                        .or_else(|| {
                            definition
                                .pc_variables
                                .get_ci(variable)
                                .map(|variable| variable.options.as_ref())
                        })
                        .map(|options| {
                            options
                                .iter()
                                .filter(|option| !option.hidden)
                                .map(|option| DialogSelectOption {
                                    value: option.value,
                                    label: option.label.clone(),
                                })
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                        .into()
                }),
        );
        let help = definition.context_help.get_ci(variable).cloned();

        (options, help)
    }

    /// Collects a dialog's rows, walking its panels in place.
    ///
    /// `visited` holds lowercased dialog names: definitions reference panels in
    /// cycles, and a dialog that appears twice in one layout is drawn once.
    /// `settingOption` rows join the selector above them; any other row closes
    /// the group. Container panels (`indicatorPanel`, `readoutPanel`,
    /// `liveGraph`) recurse like plain panels so their widget rows are carried.
    fn walk(&mut self, name: &str, section: &str) {
        if !self.visited.insert(name.to_ascii_lowercase()) {
            return;
        }
        let definition = self.project.definition();
        let Some(dialog) = definition.dialogs.get_ci(name) else {
            return;
        };

        let outer_preset = self.pending_preset;
        self.pending_preset = None;

        for row in &dialog.rows {
            match row.kind {
                RowKind::TopicHelp => {
                    self.pending_preset = None;

                    if self.topic_help.is_none() {
                        self.topic_help = row
                            .fields
                            .first()
                            .map(|field| oxtuner_ini::fields::unquote(field).trim().to_owned())
                            .filter(|link| !link.is_empty());
                    }
                }
                RowKind::Help | RowKind::WebHelp => {
                    self.pending_preset = None;
                    self.walk_help(row, section);
                }
                RowKind::Panel | RowKind::IndicatorPanel | RowKind::ReadoutPanel => {
                    // No group close: the nested dialog is its own frame and
                    // `walk` saves and restores the pending selector.
                    self.walk_panel(row, section);
                }
                RowKind::SettingSelector | RowKind::SettingOption => {
                    self.walk_preset(row);
                }
                RowKind::CommandButton => {
                    self.pending_preset = None;

                    let command = command_row(self.project, row, &self.gates);
                    self.commands.push(command);
                }
                RowKind::Gauge
                | RowKind::Indicator
                | RowKind::LiveGraph
                | RowKind::GraphLine
                | RowKind::Readout
                | RowKind::RuntimeValue => {
                    self.walk_widget(row, section);
                }
                RowKind::Field
                | RowKind::DisplayOnlyField
                | RowKind::Slider
                | RowKind::Checkbox
                | RowKind::Radio => {
                    self.pending_preset = None;

                    if let Some(field) = field_row(self, row, section) {
                        self.fields.push(field);
                    }
                }
                RowKind::Array1D | RowKind::Unknown => {
                    self.pending_preset = None;

                    self.unmodeled.push(DialogUnmodeledRow {
                        keyword: row.keyword.clone(),
                        fields: row.fields.clone(),
                    });
                }
            }
        }

        self.pending_preset = outer_preset;
    }

    /// Carries one `help` / `webHelp` row as a note or a help link.
    fn walk_help(&mut self, row: &DialogRow, section: &str) {
        if row.kind == RowKind::WebHelp {
            let topic = row
                .fields
                .first()
                .map(|field| oxtuner_ini::fields::unquote(field).trim().to_owned())
                .filter(|topic| !topic.is_empty());

            if let Some(topic) = topic {
                self.help_links.push(DialogHelpLink {
                    topic,
                    title: String::new(),
                });
            }
            return;
        }

        if row.fields.len() > 1 {
            let topic = oxtuner_ini::fields::unquote(row.fields.first().unwrap_or(&String::new()))
                .trim()
                .to_owned();
            let title = oxtuner_ini::fields::unquote(row.fields.get(1).unwrap_or(&String::new()))
                .trim()
                .to_owned();

            if topic.is_empty() {
                if !title.is_empty() {
                    self.fields
                        .push(note(title, section, false, None, None, &self.gates));
                }
            } else {
                self.help_links.push(DialogHelpLink { topic, title });
            }
        } else if let Some(field) = help_row(row, section, &self.gates) {
            self.fields.push(field);
        }
    }

    /// Groups one `settingSelector` and its `settingOption` rows.
    fn walk_preset(&mut self, row: &DialogRow) {
        if row.kind == RowKind::SettingOption {
            if let Some(index) = self.pending_preset
                && let Some(preset) = self.presets.get_mut(index)
            {
                preset.options.push(preset_option(row));
            }
            return;
        }

        let (visibility, enabled) = row_conditions(self.project, row, None);

        self.presets.push(DialogPreset {
            label: row
                .fields
                .first()
                .map(|field| oxtuner_ini::fields::unquote(field).to_owned())
                .unwrap_or_default(),
            visibility: and_gate(self.gates.visible.as_deref(), visibility),
            enabled: and_gate(self.gates.enabled.as_deref(), enabled),
            options: Vec::new(),
        });
        self.pending_preset = Some(self.presets.len() - 1);
    }

    /// Walks one `panel =` row into its target dialog, table, curve or `std_*`
    /// name.
    ///
    /// The row's gates travel with the recursion: every row the panel embeds
    /// combines them with its own.
    fn walk_panel(&mut self, row: &DialogRow, section: &str) {
        let definition = self.project.definition();

        let Some(target) = panel_target(row) else {
            return;
        };

        if let Some(nested) = definition.dialogs.get_ci(target) {
            let title = nested
                .title
                .as_deref()
                .map(str::trim)
                .filter(|title| !title.is_empty());

            let saved = self.gates.clone();
            let panel = panel_gates(row);
            self.gates.enabled = and_gate(saved.enabled.as_deref(), panel.enabled);
            self.gates.visible = and_gate(saved.visible.as_deref(), panel.visible);
            self.walk(&nested.name, title.unwrap_or(section));
            self.gates = saved;
            return;
        }

        self.record_embedded(target);

        if target.to_ascii_lowercase().starts_with("std_") {
            self.walk_std_panel(target, row);
        }
    }

    /// Composes a built-in `std_*` panel from the loaded definition.
    ///
    /// Known panels become rows under their own title; unknown ones (or ones
    /// with no usable constant) keep the muted `unavailable` name instead.
    fn walk_std_panel(&mut self, target: &str, row: &DialogRow) {
        let Some((title, rows)) = super::synthesize(self.project, target) else {
            if !self
                .unavailable
                .iter()
                .any(|name| name.eq_ignore_ascii_case(target))
            {
                self.unavailable.push(target.to_owned());
            }
            return;
        };

        let saved = self.gates.clone();
        let panel = panel_gates(row);
        self.gates.enabled = and_gate(saved.enabled.as_deref(), panel.enabled);
        self.gates.visible = and_gate(saved.visible.as_deref(), panel.visible);

        for (label, variable) in rows {
            let synthetic = DialogRow {
                kind: RowKind::Field,
                keyword: "field".to_owned(),
                fields: vec![label, variable],
                line: row.line,
            };

            if let Some(field) = field_row(self, &synthetic, title) {
                self.fields.push(field);
            }
        }

        self.gates = saved;
    }

    /// Carries one embedded widget row, recursing into graph containers.
    fn walk_widget(&mut self, row: &DialogRow, section: &str) {
        let definition = self.project.definition();

        if row.kind == RowKind::LiveGraph
            && let Some(nested) = row
                .fields
                .first()
                .and_then(|field| definition.dialogs.get_ci(field))
        {
            let title = nested
                .title
                .as_deref()
                .map(str::trim)
                .filter(|title| !title.is_empty());
            self.walk(&nested.name, title.unwrap_or(section));
            return;
        }

        self.pending_preset = None;

        if let Some(widget) = widget_row(row, section, &self.gates) {
            self.widgets.push(widget);
        }
    }
}

/// Gates a panel passes to every row it embeds.
///
/// A `panel = name, West, {enable}, {visible}` row adjusts the enabled state
/// of all components in the panel. The core carries the expressions raw and
/// combines them with each row's own gates; evaluation belongs to the UI.
#[derive(Debug, Clone, Default)]
struct InheritedGates {
    enabled: Option<String>,
    visible: Option<String>,
}

/// Combines a panel gate with a row's own gate.
///
/// A missing side is always true, so it behaves as the identity.
fn and_gate(outer: Option<&str>, inner: Option<String>) -> Option<String> {
    match (outer, inner) {
        (Some(o), Some(i)) => Some(format!("({o}) && ({i})")),
        (Some(o), None) => Some(o.to_owned()),
        (None, inner) => inner,
    }
}

/// Enable and visibility gates of one `panel =` row.
///
/// Panels take `panel = name, placement, {enable}, {visible}`: the placement
/// is a bare word, so only brace fields count, enable first.
fn panel_gates(row: &DialogRow) -> InheritedGates {
    let mut gates = row.fields.iter().skip(1).filter_map(|field| {
        let (_, trailing) = split_gate(field);
        trailing.and_then(condition_body)
    });

    let enabled = gates.next();
    let visible = gates.next();

    InheritedGates { enabled, visible }
}
/// The panel one `panel =` row embeds, without its `{ … }` gate or placement.
///
/// The dialect lets a panel carry a placement and gates (`panel = std_ms3Rtc
/// {rtc_mode}`). Only the name travels on here; [`panel_gates`] carries the
/// gates, and the gate would wrongly become part of the name.
fn panel_target(row: &DialogRow) -> Option<&str> {
    let field = row.fields.first()?;

    let unquoted = oxtuner_ini::fields::unquote(field).trim();

    let bare = unquoted
        .split(['{', ' ', '\t'])
        .next()
        .map(str::trim)
        .unwrap_or_default();

    (!bare.is_empty()).then_some(bare)
}

/// The body of a `{ … }` condition, as written.
///
/// Empty braces, `0` and `1` carry no condition: they are the dialect's way of
/// leaving a slot unused. Shared with the menu tree, whose items carry the
/// same `{enable}, {visible}` trailing expressions.
pub(crate) fn condition_body(field: &str) -> Option<String> {
    let body = oxtuner_ini::fields::expression_body(field.trim())?;

    let body = body.trim();

    if body.is_empty() || body == "0" || body == "1" {
        return None;
    }

    Some(body.to_owned())
}

/// Visibility and enable conditions of one dialog row.
///
/// The dialect writes `field = "Label", var, {enable}, {visible}`; a row with
/// one condition carries only the enable gate. Empty slots are skipped, so
/// `{ cond }, {}` reads as enable-only. Gates glued to the variable
/// (`var { cond }`, written without a comma) read positionally, and a bare
/// token that names a constant (`!useStepperIdle`, `useStepperIdle`) is an
/// enable gate too. Bare numbers and keywords (`1`, `horizontal`) are layout
/// noise and ignored.
fn row_conditions(
    project: &Project,
    row: &DialogRow,
    variable_at: Option<usize>,
) -> (Option<String>, Option<String>) {
    let mut conditions = Vec::new();

    for (index, field) in row.fields.iter().enumerate().skip(1) {
        let trimmed = field.trim();

        if let Some(body) = condition_body(trimmed) {
            conditions.push(body);
            continue;
        }

        let (token, trailing) = split_gate(trimmed);

        if let Some(body) = trailing.and_then(condition_body) {
            conditions.push(body);
        }

        if Some(index) == variable_at || token.is_empty() {
            continue;
        }

        if is_layout_noise(token) {
            continue;
        }

        if names_variable(project, token) {
            conditions.push(token.to_owned());
        }
    }

    order_gates(conditions)
}

/// Orders collected gate bodies into `(visibility, enabled)`.
///
/// The dialect writes `{enable}` first and `{visible}` second; a single gate
/// is always the enable one. Shared by dialog rows, buttons, widgets and menu
/// items so the order never drifts between surfaces.
pub(crate) fn order_gates(gates: Vec<String>) -> (Option<String>, Option<String>) {
    let mut gates = gates.into_iter();
    let first = gates.next();
    let second = gates.next();

    match (first, second) {
        (Some(enabled), Some(visible)) => (Some(visible), Some(enabled)),
        (Some(enabled), None) | (None, Some(enabled)) => (None, Some(enabled)),
        (None, None) => (None, None),
    }
}

/// Splits a field into its token and a trailing `{ … }` gate.
///
/// `ignBypassPin { ignBypassEnable }` yields (`ignBypassPin`, the gate); a
/// whole `{ cond }` field yields (`""`, the gate); anything else yields the
/// field and no gate.
fn split_gate(field: &str) -> (&str, Option<&str>) {
    let trimmed = oxtuner_ini::fields::unquote(field).trim();

    if trimmed.starts_with('{') {
        return ("", Some(trimmed));
    }

    match trimmed.find('{') {
        Some(open) => (trimmed[..open].trim(), Some(trimmed[open..].trim())),
        None => (trimmed, None),
    }
}

/// Whether a bare token is layout noise rather than a gate.
///
/// Sliders carry `horizontal`; some generators emit a bare `1`. Neither names
/// a condition.
fn is_layout_noise(token: &str) -> bool {
    token.eq_ignore_ascii_case("horizontal") || token.parse::<f64>().is_ok()
}

/// Whether a bare token names a tune constant or PC variable.
///
/// A leading `!` negates the gate but still names the variable.
fn names_variable(project: &Project, token: &str) -> bool {
    let name = token.strip_prefix('!').unwrap_or(token).trim();

    !name.is_empty() && project.layout().variable(name).is_some()
}

/// One `help` row as a note.
///
/// Two-field rows are help links and never reach here; the walk files them
/// under [`DialogSummary::help_links`] instead.
fn help_row(row: &DialogRow, section: &str, gates: &InheritedGates) -> Option<DialogFieldSummary> {
    let text = row
        .fields
        .first()
        .map(|field| oxtuner_ini::fields::unquote(field).trim().to_owned())
        .filter(|text| !text.is_empty())?;

    Some(note(text, section, false, None, None, gates))
}

/// The variable reference one dialog row writes, when it names one.
///
/// Most rows carry `field = "Label", variable`; a row whose own label is a
/// variable name resolves through the same lookup. A gate glued to the
/// variable (`var { cond }`) is split off first, and an array element
/// (`var[2]`) resolves to its base variable plus index. `label_at` is the
/// label's slot: a `radio` row leads with its orientation, so the search
/// starts one slot later.
fn resolve_reference(
    row: &DialogRow,
    project: &Project,
    label_at: usize,
) -> Option<VariableReference> {
    let indexed = row
        .fields
        .iter()
        .enumerate()
        .skip(label_at + 1)
        .find_map(|(index, field)| {
            let (token, _) = split_gate(field);

            if token.is_empty() {
                return None;
            }

            let (base, element) = split_element(token)?;

            project
                .layout()
                .variable(base)
                .is_some()
                .then(|| (index, base.to_owned(), element))
        });

    if let Some((field, variable, index)) = indexed {
        return Some(VariableReference {
            field,
            variable,
            index,
        });
    }

    let first = row
        .fields
        .get(label_at)
        .map(|field| oxtuner_ini::fields::unquote(field).trim())
        .unwrap_or_default();
    let (token, _) = split_gate(first);

    if token.is_empty() {
        return None;
    }

    let (base, element) = split_element(token)?;

    (!base.is_empty() && project.layout().variable(base).is_some()).then(|| VariableReference {
        field: label_at,
        variable: base.to_owned(),
        index: element,
    })
}

/// One resolved variable reference: the base name plus an array element.
struct VariableReference {
    /// Position of the naming field within the row.
    field: usize,
    /// Base variable name, without any `[i]` suffix.
    variable: String,
    /// Array element (`var[i]`), none for whole variables.
    index: Option<usize>,
}

/// Splits an array element reference into its base and index.
///
/// Plain names yield no index; a malformed suffix yields no reference at all
/// rather than a guess.
fn split_element(token: &str) -> Option<(&str, Option<usize>)> {
    let token = token.trim();

    let Some(open) = token.find('[') else {
        return Some((token, None));
    };

    let base = token[..open].trim();
    let index = token[open + 1..].strip_suffix(']')?.trim().parse().ok()?;

    (!base.is_empty()).then_some((base, Some(index)))
}

/// One `settingOption` row as a preset option.
fn preset_option(row: &DialogRow) -> DialogPresetOption {
    DialogPresetOption {
        label: row
            .fields
            .first()
            .map(|field| oxtuner_ini::fields::unquote(field).to_owned())
            .unwrap_or_default(),
        assignments: row
            .fields
            .iter()
            .skip(1)
            .filter_map(|field| {
                let (name, value) = field.split_once('=')?;
                let variable = name.trim();

                (!variable.is_empty()).then(|| DialogPresetAssignment {
                    variable: variable.to_owned(),
                    value: oxtuner_ini::fields::unquote(value.trim()).to_owned(),
                })
            })
            .collect(),
    }
}

/// One `commandButton` row as a command summary.
///
/// The row carries `commandButton = "Label", command, {enable}, flags…`: one
/// enable gate at most, then bare flags. A bare token that names a constant is
/// an enable gate instead; numbers are skipped.
fn command_row(project: &Project, row: &DialogRow, gates: &InheritedGates) -> DialogCommand {
    let command = row
        .fields
        .get(1)
        .map(|field| oxtuner_ini::fields::unquote(field).trim().to_owned())
        .unwrap_or_default();

    let mut flags = Vec::new();
    let mut found = Vec::new();

    for field in row.fields.iter().skip(2) {
        let trimmed = field.trim();

        if let Some(body) = condition_body(trimmed) {
            found.push(body);
            continue;
        }

        let (token, trailing) = split_gate(trimmed);

        if let Some(body) = trailing.and_then(condition_body) {
            found.push(body);
        }

        if token.is_empty() || token.parse::<f64>().is_ok() {
            continue;
        }

        if names_variable(project, token) {
            found.push(token.to_owned());
            continue;
        }

        flags.push(oxtuner_ini::fields::unquote(token).to_owned());
    }

    let (visibility, enabled) = order_gates(found);

    DialogCommand {
        label: row
            .fields
            .first()
            .map(|field| oxtuner_ini::fields::unquote(field).to_owned())
            .unwrap_or_default(),
        command: command.clone(),
        visibility: and_gate(gates.visible.as_deref(), visibility),
        enabled: and_gate(gates.enabled.as_deref(), enabled),
        known: !command.is_empty()
            && project
                .definition()
                .controller_commands
                .get_ci(&command)
                .is_some(),
        flags,
    }
}

/// One embedded widget row as data.
fn widget_row(row: &DialogRow, section: &str, gates: &InheritedGates) -> Option<DialogWidget> {
    let (kind, target_at) = match row.kind {
        RowKind::Gauge => (DialogWidgetKind::Gauge, 0),
        RowKind::Indicator => (DialogWidgetKind::Indicator, 0),
        RowKind::LiveGraph => (DialogWidgetKind::LiveGraph, 0),
        RowKind::GraphLine => (DialogWidgetKind::GraphLine, 0),
        RowKind::Readout => (DialogWidgetKind::Readout, 0),
        RowKind::RuntimeValue => (DialogWidgetKind::RuntimeValue, 1),
        _ => return None,
    };

    let target = row
        .fields
        .get(target_at)
        .map(|field| {
            if row.kind == RowKind::Indicator {
                condition_body(field.trim())
                    .unwrap_or_else(|| oxtuner_ini::fields::unquote(field).trim().to_owned())
            } else {
                oxtuner_ini::fields::unquote(field).trim().to_owned()
            }
        })
        .filter(|target| !target.is_empty())?;

    let label = match row.kind {
        RowKind::Indicator => row
            .fields
            .get(2)
            .or_else(|| row.fields.get(1))
            .map(|field| oxtuner_ini::fields::unquote(field).to_owned())
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| target.clone()),
        RowKind::RuntimeValue | RowKind::Readout | RowKind::GraphLine => row
            .fields
            .get(1)
            .map(|field| oxtuner_ini::fields::unquote(field).to_owned())
            .filter(|label| !label.trim().is_empty() && *label != target)
            .unwrap_or_else(|| target.clone()),
        _ => target.clone(),
    };

    let found = row.fields.iter().skip(1).filter_map(|field| {
        let (_, trailing) = split_gate(field);
        trailing.and_then(condition_body)
    });
    let (visibility, enabled) = order_gates(found.collect());

    let (units, minimum, maximum) = widget_limits(row);

    Some(DialogWidget {
        kind,
        label,
        target,
        visibility: and_gate(gates.visible.as_deref(), visibility),
        enabled: and_gate(gates.enabled.as_deref(), enabled),
        section: section.to_owned(),
        units,
        minimum,
        maximum,
    })
}

/// Display limits of a `graphLine` or full-spec `readout` row.
///
/// A `graphLine` carries `channel, units, min, max`; a full `readout` carries
/// `variable, Title, Units, Lo, Hi, …`. Anything else (`auto`, an expression,
/// or nothing) yields no limit: the app falls back to the gauge template.
fn widget_limits(row: &DialogRow) -> (String, Option<f64>, Option<f64>) {
    let at = match row.kind {
        RowKind::GraphLine => 1,
        RowKind::Readout if row.fields.len() > 3 => 2,
        _ => return (String::new(), None, None),
    };

    let units = row
        .fields
        .get(at)
        .map(|field| oxtuner_ini::fields::unquote(field).trim().to_owned())
        .unwrap_or_default();

    let number = |index: usize| {
        row.fields
            .get(index)
            .map(|field| oxtuner_ini::fields::unquote(field).trim().to_owned())
            .and_then(|token| token.parse().ok())
    };

    (units, number(at + 1), number(at + 2))
}

/// A label-only row as a note, splitting the `!` / `#` warning marker.
///
/// Conditional notes (`field = "!Warning", {}, { cond }`) keep their gates so
/// the UI can hide them like any other row.
fn note_field(
    project: &Project,
    row: &DialogRow,
    label: String,
    section: &str,
    gates: &InheritedGates,
) -> Option<DialogFieldSummary> {
    if label.is_empty() {
        return None;
    }

    let warning = matches!(label.chars().next(), Some('!' | '#'));

    let text = if warning {
        label.get(1..).map(str::trim).unwrap_or_default().to_owned()
    } else {
        label
    };

    if text.is_empty() {
        return None;
    }

    let (visibility, enabled) = row_conditions(project, row, None);

    Some(note(text, section, warning, visibility, enabled, gates))
}

/// One note row: definition text with no variable behind it.
fn note(
    text: String,
    section: &str,
    warning: bool,
    visibility: Option<String>,
    enabled: Option<String>,
    gates: &InheritedGates,
) -> DialogFieldSummary {
    DialogFieldSummary {
        label: text.clone(),
        variable: String::new(),
        value: None,
        text: Some(text),
        units: String::new(),
        digits: 0,
        changed: false,
        display_only: true,
        writable: false,
        kind: DialogFieldKind::Note,
        section: section.to_owned(),
        warning,
        options: Arc::default(),
        visibility: and_gate(gates.visible.as_deref(), visibility),
        enabled: and_gate(gates.enabled.as_deref(), enabled),
        help: None,
        minimum: None,
        maximum: None,
        index: None,
        hex: false,
    }
}

/// One readable row of a dialog.
///
/// The variable is the first field after the label that names a tune constant;
/// a row with no variable is text the definition shows (a hint or a warning),
/// not a value. A `radio` row leads with its orientation, so its label and
/// variable sit one slot later.
#[expect(
    clippy::float_cmp,
    reason = "a field is changed only when its stored raw value differs exactly"
)]
fn field_row(walk: &mut RowWalk<'_>, row: &DialogRow, section: &str) -> Option<DialogFieldSummary> {
    let project = walk.project;

    // `radio = orientation, "Label", bitConst, …`: the orientation is not the
    // label and the label is not the variable.
    let label_at = usize::from(row.kind == RowKind::Radio);

    let label = row
        .fields
        .get(label_at)
        .map(|field| oxtuner_ini::fields::unquote(field).to_owned())
        .unwrap_or_default();

    let tune = project.tune();
    let reference = resolve_reference(row, project, label_at);

    let Some(reference) = reference else {
        return note_field(project, row, label, section, &walk.gates);
    };

    let variable = reference.variable.as_str();
    let at = reference.index.unwrap_or(0);
    let spec = tune.variable(variable).ok()?;
    let baseline = project.images().baseline();
    // Whole arrays read as tables, not rows: only an explicit `var[i]`
    // reference (or a scalar) carries a value here.
    let single = reference.index.is_some() || spec.shape.is_scalar();
    let (value, text) = if single && spec.shape.is_numeric() {
        (tune.user_at(variable, at).ok(), None)
    } else if !spec.shape.is_numeric() {
        (None, tune.text(variable).ok().map(str::to_owned))
    } else {
        (None, None)
    };
    let changed = if single && spec.shape.is_numeric() {
        matches!(
            (tune.raw_at(variable, at), baseline.raw_at(variable, at)),
            (Ok(current), Ok(saved)) if current != saved
        )
    } else {
        false
    };
    let display_only = row.kind == RowKind::DisplayOnlyField;
    let writable =
        !display_only && !project.is_read_only() && tune.is_writable(variable) && value.is_some();

    let (visibility, enabled) = row_conditions(project, row, Some(reference.field));
    let (options, help) = walk.presentation(variable);

    let kind = match row.kind {
        RowKind::Checkbox | RowKind::Radio => DialogFieldKind::Toggle,
        RowKind::Slider => DialogFieldKind::Slider,
        _ if !options.is_empty() => DialogFieldKind::Select,
        _ => DialogFieldKind::Value,
    };

    Some(DialogFieldSummary {
        label: if label.is_empty() {
            variable.to_owned()
        } else {
            label
        },
        variable: variable.to_owned(),
        value,
        text,
        units: if spec.shape.is_numeric() {
            spec.units.clone()
        } else {
            String::new()
        },
        digits: spec.digits,
        changed,
        display_only,
        writable,
        kind,
        section: section.to_owned(),
        warning: false,
        options,
        visibility: and_gate(walk.gates.visible.as_deref(), visibility),
        enabled: and_gate(walk.gates.enabled.as_deref(), enabled),
        help,
        minimum: spec.minimum,
        maximum: spec.maximum,
        index: reference.index,
        hex: has_hex_flag(row),
    })
}

/// Whether the row asks for hexadecimal display (`displayInHex`).
///
/// The flag is a bare fifth field (`field = "Label", var, {en}, {vis},
/// displayInHex`); braces and the variable never match it.
fn has_hex_flag(row: &DialogRow) -> bool {
    row.fields.iter().any(|field| {
        let (token, trailing) = split_gate(field);

        trailing.is_none() && token.eq_ignore_ascii_case("displayinhex")
    })
}
