use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use oxtuner_ini::{DialogRow, RowKind};
use oxtuner_project::Project;

use crate::contract::GateState;
use crate::contract::editor::dialog::{
    DialogAction, DialogEmbed, DialogField, DialogFieldKind, DialogFieldTone, DialogHelpLink,
    DialogPreset, DialogPresetAssignment, DialogPresetOption, DialogSelectOption,
    DialogUnmodeledRow, DialogView, DialogWidget, DialogWidgetKind,
};
use crate::gate::{GateEvaluator, resolve_gates};
use crate::realtime::RealtimeFrame;

use super::std_panels::synthesize;

impl DialogView {
    /// Reads one dialog's rows from an open project, panels flattened.
    ///
    /// A dialog is a layout: `Engine Constants` is a border of panels, and the
    /// constants sit one or two levels down. Reading only its own rows would
    /// show an empty list, so panels are walked in place and their rows keep the
    /// nested title as a section. Rows that name no tune variable are the
    /// definition's own text, kept as notes. A known `std_*` panel composes
    /// from the definition's constants; an unknown one is reported by name.
    ///
    /// Tables and curves a panel embeds are grids, not rows: the menu opens
    /// them, and only the grid's snapshot carries their values.
    pub(crate) fn from_project(
        project: &Project,
        name: &str,
        live: Option<&RealtimeFrame>,
    ) -> Result<Self, String> {
        let definition = project.definition();
        let dialog = definition
            .dialogs
            .get_ci(name)
            .ok_or_else(|| format!("the definition has no dialog named `{name}`"))?;

        let mut walk = RowWalk::new(project);
        walk.walk(&dialog.name, "");

        let mut finished = walk.finish();
        let mut evaluator = GateEvaluator::new(definition, project.tune(), live);
        finished.resolve(&mut evaluator);

        let unresolved: Vec<&str> = evaluator.unresolved().collect();
        if !unresolved.is_empty() {
            tracing::debug!(
                dialog = %dialog.name,
                unresolved = ?unresolved,
                "dialog gates could not be evaluated"
            );
        }

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
            actions: finished.actions,
            widgets: finished.widgets,
            tables: finished.tables,
            curves: finished.curves,
            unmodeled: finished.unmodeled,
        })
    }
}

/// The collected rows of one dialog walk.
struct FinishedWalk {
    fields: Vec<DialogField>,
    unavailable: Vec<String>,
    topic_help: Option<String>,
    help_links: Vec<DialogHelpLink>,
    presets: Vec<DialogPreset>,
    actions: Vec<DialogAction>,
    widgets: Vec<DialogWidget>,
    tables: Vec<DialogEmbed>,
    curves: Vec<DialogEmbed>,
    unmodeled: Vec<DialogUnmodeledRow>,
}

/// The accumulators of one dialog walk.
///
/// Panels recurse, so every row a dialog flattens into travels together
/// instead of as one long argument list.
struct RowWalk<'project> {
    project: &'project Project,
    visited: HashSet<String>,
    /// How many dialogs deep the walk is. `topicHelp` belongs to the dialog
    /// that declares it, not to the panel it is embedded in.
    depth: usize,
    gates: InheritedGates,
    fields: Vec<DialogField>,
    unavailable: Vec<String>,
    topic_help: Option<String>,
    help_links: Vec<DialogHelpLink>,
    presets: Vec<DialogPreset>,
    pending_preset: Option<usize>,
    actions: Vec<DialogAction>,
    widgets: Vec<DialogWidget>,
    tables: Vec<DialogEmbed>,
    curves: Vec<DialogEmbed>,
    unmodeled: Vec<DialogUnmodeledRow>,
    options_cache: HashMap<String, Arc<[DialogSelectOption]>>,
}

impl FinishedWalk {
    /// Resolves every gate the walk carried against the working tune.
    fn resolve(&mut self, evaluator: &mut GateEvaluator<'_>) {
        for field in &mut self.fields {
            field.gates = resolve_gates(
                evaluator,
                field.visibility_expression.as_deref(),
                field.enable_expression.as_deref(),
            );
        }

        for preset in &mut self.presets {
            preset.gates = resolve_gates(
                evaluator,
                preset.visibility_expression.as_deref(),
                preset.enable_expression.as_deref(),
            );
        }

        for action in &mut self.actions {
            action.gates = resolve_gates(
                evaluator,
                action.visibility_expression.as_deref(),
                action.enable_expression.as_deref(),
            );
        }

        for widget in &mut self.widgets {
            widget.gates = resolve_gates(
                evaluator,
                widget.visibility_expression.as_deref(),
                widget.enable_expression.as_deref(),
            );
        }

        for embed in self.tables.iter_mut().chain(self.curves.iter_mut()) {
            embed.gates = resolve_gates(
                evaluator,
                embed.visibility_expression.as_deref(),
                embed.enable_expression.as_deref(),
            );
        }
    }
}

impl<'project> RowWalk<'project> {
    fn new(project: &'project Project) -> Self {
        Self {
            project,
            visited: HashSet::new(),
            depth: 0,
            gates: InheritedGates::default(),
            fields: Vec::new(),
            unavailable: Vec::new(),
            topic_help: None,
            help_links: Vec::new(),
            presets: Vec::new(),
            pending_preset: None,
            actions: Vec::new(),
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
            actions: self.actions,
            widgets: self.widgets,
            tables: self.tables,
            curves: self.curves,
            unmodeled: self.unmodeled,
        }
    }

    /// Records an embedded table or curve once, in document order.
    ///
    /// The panel row's gates are combined with the gates it inherits, so a
    /// table inside a gated panel keeps the parent condition too. The first
    /// occurrence wins when a dialog names the same embed twice.
    fn record_embedded(&mut self, target: &str, section: &str, row: &DialogRow) {
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
            if !self.tables.iter().any(|embed| embed.id == table) {
                let embed = self.embed_row(table, section, row);
                self.tables.push(embed);
            }
            return;
        }

        if let Some(curve) = definition
            .curves
            .get_ci(target)
            .map(|curve| curve.name.clone())
            && !self.curves.iter().any(|embed| embed.id == curve)
        {
            let embed = self.embed_row(curve, section, row);
            self.curves.push(embed);
        }
    }

    /// Builds one embed row with the panel's gates folded in.
    fn embed_row(&self, id: String, section: &str, row: &DialogRow) -> DialogEmbed {
        let panel = panel_gates(self.project, row);

        DialogEmbed {
            id,
            section: section.to_owned(),
            visibility_expression: and_gate(self.gates.visible.as_deref(), panel.visible),
            enable_expression: and_gate(self.gates.enabled.as_deref(), panel.enabled),
            gates: GateState::default(),
        }
    }

    /// Select options and help text of one variable, from the definition.
    ///
    /// Bits constants and bit PC variables carry their drop-down list here;
    /// every other class yields no options. Entries the dialect marks `INVALID`
    /// are skipped, and one `Arc` per variable is shared by every row that
    /// names it. Help comes from `[SettingContextHelp]`.
    fn presentation(&mut self, variable: &str) -> (Arc<[DialogSelectOption]>, Option<String>) {
        let definition = self.project.definition();

        let options = if let Some(options) = self.options_cache.get(variable) {
            Arc::clone(options)
        } else {
            let options: Arc<[DialogSelectOption]> = definition
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
                .into();

            self.options_cache
                .insert(variable.to_owned(), Arc::clone(&options));

            options
        };

        let help = definition.context_help.get_ci(variable).cloned();

        (options, help)
    }

    /// Collects a dialog's rows, walking its panels in place.
    ///
    /// `visited` holds lowercased dialog names: definitions reference panels in
    /// cycles, and a dialog that appears twice in one layout is drawn once.
    /// `settingOption` rows join the selector above them; a terminal row closes
    /// the group, while a panel keeps it open across the nested walk. Container
    /// panels (`indicatorPanel`, `readoutPanel`, `liveGraph`) recurse like plain
    /// panels so their widget rows are carried.
    fn walk(&mut self, name: &str, section: &str) {
        if !self.visited.insert(name.to_ascii_lowercase()) {
            return;
        }
        let definition = self.project.definition();
        let Some(dialog) = definition.dialogs.get_ci(name) else {
            return;
        };

        let is_top = self.depth == 0;
        self.depth += 1;

        let outer_preset = self.pending_preset;
        self.pending_preset = None;

        for row in &dialog.rows {
            match row.kind {
                RowKind::TopicHelp => {
                    self.pending_preset = None;

                    if is_top && self.topic_help.is_none() {
                        self.topic_help = row
                            .fields
                            .first()
                            .map(|field| oxtuner_ini::fields::unquote(field).trim().to_owned())
                            .filter(|link| !link.is_empty());
                    }
                }
                RowKind::HelpText | RowKind::WebHelp => {
                    self.pending_preset = None;
                    self.walk_help(row, section);
                }
                RowKind::EmbeddedPanel | RowKind::IndicatorPanel | RowKind::ReadoutPanel => {
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
                    self.actions.push(command);
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
                RowKind::Array1D | RowKind::Unmodeled => {
                    self.pending_preset = None;

                    self.unmodeled.push(DialogUnmodeledRow {
                        keyword: row.keyword.clone(),
                        fields: row.fields.clone(),
                    });
                }
            }
        }

        self.pending_preset = outer_preset;
        self.depth -= 1;
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
            let topic = row.fields.first().map_or("", String::as_str);
            let title = row.fields.get(1).map_or("", String::as_str);
            let topic = oxtuner_ini::fields::unquote(topic).trim().to_owned();
            let title = oxtuner_ini::fields::unquote(title).trim().to_owned();

            if topic.is_empty() {
                if !title.is_empty() {
                    self.fields.push(note(
                        title,
                        section,
                        DialogFieldTone::Normal,
                        None,
                        None,
                        &self.gates,
                    ));
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
                preset.options.push(preset_option(self.project, row));
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
            visibility_expression: and_gate(self.gates.visible.as_deref(), visibility),
            enable_expression: and_gate(self.gates.enabled.as_deref(), enabled),
            gates: GateState::default(),
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
            let panel = panel_gates(self.project, row);
            self.gates.enabled = and_gate(saved.enabled.as_deref(), panel.enabled);
            self.gates.visible = and_gate(saved.visible.as_deref(), panel.visible);
            self.walk(&nested.name, title.unwrap_or(section));
            self.gates = saved;
            return;
        }

        self.record_embedded(target, section, row);

        if target.to_ascii_lowercase().starts_with("std_") {
            self.walk_std_panel(target, row);
        }
    }

    /// Composes a built-in `std_*` panel from the loaded definition.
    ///
    /// Known panels become rows under their own title; unknown ones (or ones
    /// with no usable constant) keep the muted `unavailable` name instead.
    fn walk_std_panel(&mut self, target: &str, row: &DialogRow) {
        if !self
            .visited
            .insert(format!("std:{}", target.to_ascii_lowercase()))
        {
            return;
        }

        let Some((title, rows)) = synthesize(self.project, target) else {
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
        let panel = panel_gates(self.project, row);
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

        if let Some(widget) = widget_row(self.project, row, section, &self.gates) {
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

/// Enable and visibility gates of one panel row.
///
/// `panel` takes `name, placement, {enable}, {visible}`. The container rows
/// take a count before their single enable gate (`indicatorPanel` and
/// `readoutPanel`), so the gate start depends on the row kind.
fn panel_gates(project: &Project, row: &DialogRow) -> InheritedGates {
    // A gate may be glued to the panel name: `panel = std_ms3Rtc {rtc_mode}`.
    let glued = row.fields.first().and_then(|field| split_gate(field).1);

    let start = match row.kind {
        RowKind::IndicatorPanel | RowKind::ReadoutPanel => 2,
        _ => 1,
    };

    let slots = collect_gates(
        project,
        glued
            .into_iter()
            .chain(row.fields.iter().skip(start).map(String::as_str)),
    );
    let (visible, enabled) = order_gates(slots);

    InheritedGates { enabled, visible }
}

/// The panel one `panel =` row embeds, without its `{ … }` gate or placement.
///
/// The dialect lets a panel carry a placement and gates (`panel = std_ms3Rtc
/// {rtc_mode}`). Only the name travels on here; [`panel_gates`] carries the
/// gates, and the gate would wrongly become part of the name.
pub(crate) fn panel_target(row: &DialogRow) -> Option<&str> {
    let field = row.fields.first()?;

    let unquoted = oxtuner_ini::fields::unquote(field).trim();

    let bare = unquoted
        .split('{')
        .next()
        .map(str::trim)
        .unwrap_or_default();

    (!bare.is_empty()).then_some(bare)
}

/// One expression field in a gate position.
pub(crate) enum ExpressionSlot {
    /// Empty braces or `{ 1 }`: the dialect's way of leaving the slot unused.
    Placeholder,
    /// The expression, as written.
    Condition(String),
}

impl ExpressionSlot {
    /// The condition, or `None` for a placeholder.
    pub(crate) fn into_condition(self) -> Option<String> {
        match self {
            Self::Placeholder => None,
            Self::Condition(condition) => Some(condition),
        }
    }
}

/// The body of any non-empty `{ … }` expression, as written.
///
/// Used where the braces are punctuation rather than a gate slot, such as an
/// indicator's target expression or a dynamic label.
fn expression_source(field: &str) -> Option<String> {
    let body = oxtuner_ini::fields::expression_body(field.trim())?.trim();

    (!body.is_empty()).then(|| body.to_owned())
}

/// One expression field as a gate slot.
///
/// `None` means the field is not an expression at all. Keeping an empty
/// placeholder is what makes `var, {}, {visible}` read as visibility instead
/// of shifting the visible expression into the enable slot.
pub(crate) fn expression_slot(field: &str) -> Option<ExpressionSlot> {
    let body = oxtuner_ini::fields::expression_body(field.trim())?;

    let body = body.trim();

    if body.is_empty() || body == "1" {
        Some(ExpressionSlot::Placeholder)
    } else {
        Some(ExpressionSlot::Condition(body.to_owned()))
    }
}

/// One field after a row's payload, as a gate slot or not.
enum GateField {
    /// The field is a condition or an empty placeholder.
    Gate(Option<String>),
    /// Placement, a flag, or text: not part of the gate sequence.
    Other,
}

/// Reads one field after a row's payload as a gate slot.
fn gate_field(project: &Project, field: &str) -> GateField {
    if let Some(slot) = expression_slot(field) {
        return GateField::Gate(slot.into_condition());
    }

    let (token, trailing) = split_gate(field);

    // `var { cond }` glues the condition to the variable. The token is the
    // payload, so only the trailing expression is a gate.
    if let Some(slot) = trailing.and_then(expression_slot) {
        return GateField::Gate(slot.into_condition());
    }

    let token = token.trim();

    if is_orientation(token) {
        return GateField::Other;
    }

    if token.is_empty() || token.parse::<f64>().is_ok() {
        return GateField::Gate(None);
    }

    if names_variable(project, token) {
        return GateField::Gate(Some(token.to_owned()));
    }

    GateField::Other
}

/// Collects the gate slots of the fields after a row's payload.
fn collect_gates<'field>(
    project: &Project,
    fields: impl Iterator<Item = &'field str>,
) -> Vec<Option<String>> {
    fields
        .filter_map(|field| match gate_field(project, field) {
            GateField::Gate(slot) => Some(slot),
            GateField::Other => None,
        })
        .collect()
}

/// Visibility and enable conditions of one dialog row.
///
/// The dialect writes `field = "Label", var, {enable}, {visible}`. Empty
/// placeholders keep their slot, so `var, {}, {visible}` is a visibility gate
/// and `var, {enable}` is an enable gate. A row with no variable may carry
/// empty slots before its gates; leading placeholders are dropped so the last
/// two slots still read as enable then visible. A bare token that names a
/// constant or channel (`!useStepperIdle`) is an enable gate too.
fn row_conditions(
    project: &Project,
    row: &DialogRow,
    variable_at: Option<usize>,
) -> (Option<String>, Option<String>) {
    let (glued, start) = if let Some(at) = variable_at {
        // A gate glued to the variable is the first gate slot.
        let glued = row.fields.get(at).and_then(|field| split_gate(field).1);

        (glued, at + 1)
    } else {
        // No variable: gates follow the label.
        (None, usize::from(row.kind == RowKind::Radio) + 1)
    };

    let slots = collect_gates(
        project,
        glued
            .into_iter()
            .chain(row.fields.iter().skip(start).map(String::as_str)),
    );

    order_gates(slots)
}

/// Orders collected gate slots into `(visibility, enabled)`.
///
/// The dialect writes `{enable}` first and `{visible}` second; a single gate
/// is the enable one. A row without a variable can carry leading empty slots
/// before its gates, so a leading placeholder is dropped while more than two
/// slots remain. Shared by dialog rows, buttons, widgets, panels and menu
/// items so the order never drifts between surfaces.
pub(crate) fn order_gates(mut slots: Vec<Option<String>>) -> (Option<String>, Option<String>) {
    if slots.is_empty() {
        return (None, None);
    }

    while slots.len() > 2 && slots.first().is_some_and(Option::is_none) {
        slots.remove(0);
    }

    let mut slots = slots.into_iter();
    let first = slots.next().flatten();
    let second = slots.next().flatten();

    match (first, second) {
        (Some(enabled), Some(visible)) => (Some(visible), Some(enabled)),
        (Some(enabled), None) => (None, Some(enabled)),
        (None, Some(visible)) => (Some(visible), None),
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

/// Whether a bare token is a slider or radio orientation.
///
/// The orientation sits before any gate and is not a placeholder, so it must
/// not shift the enable and visible slots.
fn is_orientation(token: &str) -> bool {
    token.eq_ignore_ascii_case("horizontal") || token.eq_ignore_ascii_case("vertical")
}

/// Whether a bare token names a tune constant, PC variable or output channel.
///
/// The dialect allows a bare condition to name any of them. A leading `!`
/// negates the gate but still names the variable.
fn names_variable(project: &Project, token: &str) -> bool {
    let name = token.strip_prefix('!').unwrap_or(token).trim();

    !name.is_empty()
        && (project.layout().variable(name).is_some()
            || project.definition().channels.contains(name))
}

/// One `help` row as a note.
///
/// Two-field rows are help links and never reach here; the walk files them
/// under [`DialogView::help_links`] instead.
fn help_row(row: &DialogRow, section: &str, gates: &InheritedGates) -> Option<DialogField> {
    let (text, tone) = split_label(row.fields.first()?);

    if text.is_empty() {
        return None;
    }

    Some(note(text, section, tone, None, None, gates))
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
fn preset_option(project: &Project, row: &DialogRow) -> DialogPresetOption {
    let assignments: Vec<DialogPresetAssignment> = row
        .fields
        .iter()
        .skip(1)
        .filter_map(|field| {
            let (name, value) = field.split_once('=')?;
            let variable = name.trim();

            (!variable.is_empty()).then(|| DialogPresetAssignment {
                variable: variable.to_owned(),
                value: oxtuner_ini::fields::unquote(value.trim()).to_owned(),
                power_cycle: project
                    .definition()
                    .extensions
                    .requires_power_cycle(variable),
            })
        })
        .collect();
    let power_cycle = assignments.iter().any(|assignment| assignment.power_cycle);

    DialogPresetOption {
        label: row
            .fields
            .first()
            .map(|field| oxtuner_ini::fields::unquote(field).to_owned())
            .unwrap_or_default(),
        assignments,
        power_cycle,
    }
}

/// One `commandButton` row as a command summary.
///
/// The row carries `commandButton = "Label", command, {enable}, flags…`: one
/// enable gate at most, then bare flags. A bare token that names a constant is
/// an enable gate instead; numbers are skipped.
fn command_row(project: &Project, row: &DialogRow, gates: &InheritedGates) -> DialogAction {
    let command = row
        .fields
        .get(1)
        .map(|field| oxtuner_ini::fields::unquote(field).trim().to_owned())
        .unwrap_or_default();

    let mut flags = Vec::new();
    let mut slots = Vec::new();

    for field in row.fields.iter().skip(2) {
        match gate_field(project, field) {
            GateField::Gate(slot) => slots.push(slot),
            GateField::Other => {
                let (token, _) = split_gate(field);
                let token = token.trim();

                if !token.is_empty() {
                    flags.push(oxtuner_ini::fields::unquote(token).to_owned());
                }
            }
        }
    }

    let (visibility, enabled) = order_gates(slots);

    DialogAction {
        label: row
            .fields
            .first()
            .map(|field| oxtuner_ini::fields::unquote(field).to_owned())
            .unwrap_or_default(),
        command: command.clone(),
        visibility_expression: and_gate(gates.visible.as_deref(), visibility),
        enable_expression: and_gate(gates.enabled.as_deref(), enabled),
        gates: GateState::default(),
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
fn widget_row(
    project: &Project,
    row: &DialogRow,
    section: &str,
    gates: &InheritedGates,
) -> Option<DialogWidget> {
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
                expression_source(field)
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
            .map(|field| widget_label(field))
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| target.clone()),
        RowKind::RuntimeValue => row
            .fields
            .first()
            .map(|field| widget_label(field))
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| target.clone()),
        RowKind::Readout => row
            .fields
            .get(1)
            .map(|field| widget_label(field))
            .filter(|label| !label.trim().is_empty() && *label != target)
            .unwrap_or_else(|| target.clone()),
        _ => target.clone(),
    };

    // Only the rows that document gates carry them: a `runtimeValue` is field
    // syntax, and an `indicator` takes its optional enable after the colours.
    // Scanning every field would read a dynamic indicator label expression as
    // a gate.
    let gate_start = match row.kind {
        RowKind::RuntimeValue => 2,
        RowKind::Indicator => 7,
        _ => usize::MAX,
    };
    let slots = collect_gates(
        project,
        row.fields.iter().skip(gate_start).map(String::as_str),
    );
    let (visibility, enabled) = order_gates(slots);

    let (units, minimum, maximum) = widget_limits(row);

    Some(DialogWidget {
        kind,
        label,
        target,
        visibility_expression: and_gate(gates.visible.as_deref(), visibility),
        enable_expression: and_gate(gates.enabled.as_deref(), enabled),
        gates: GateState::default(),
        section: section.to_owned(),
        units,
        minimum,
        maximum,
    })
}

/// A widget label, unwrapping a dynamic label expression.
///
/// An `indicator` may carry `{ Fuel cut: … }` as its on-label. The source is
/// kept without the braces so the UI can evaluate it later instead of showing
/// the expression punctuation as text.
fn widget_label(field: &str) -> String {
    let field = field.trim();

    expression_source(field).unwrap_or_else(|| oxtuner_ini::fields::unquote(field).to_owned())
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

/// Splits a `!` / `#` label marker from the text.
///
/// The marker may sit inside the quotes (`"!Red label"`, the documented form)
/// or outside them (`!"No PWM Fan available on MCU"`, which Speeduino writes).
fn split_label(label: &str) -> (String, DialogFieldTone) {
    let unquoted = oxtuner_ini::fields::unquote(label.trim());

    let (tone, text) = if let Some(text) = unquoted.strip_prefix('!') {
        (DialogFieldTone::Danger, text)
    } else if let Some(text) = unquoted.strip_prefix('#') {
        (DialogFieldTone::Info, text)
    } else {
        (DialogFieldTone::Normal, unquoted)
    };

    let text = oxtuner_ini::fields::unquote(text.trim()).trim().to_owned();

    (text, tone)
}

/// A label-only row as a note.
///
/// Conditional notes (`field = "!Warning", {}, { cond }`) keep their gates so
/// the UI can hide them like any other row.
fn note_field(
    project: &Project,
    row: &DialogRow,
    label: &str,
    section: &str,
    gates: &InheritedGates,
) -> Option<DialogField> {
    let (text, tone) = split_label(label);

    if text.is_empty() {
        return None;
    }

    let (visibility, enabled) = row_conditions(project, row, None);

    Some(note(text, section, tone, visibility, enabled, gates))
}

/// One note row: definition text with no variable behind it.
fn note(
    text: String,
    section: &str,
    tone: DialogFieldTone,
    visibility: Option<String>,
    enabled: Option<String>,
    gates: &InheritedGates,
) -> DialogField {
    DialogField {
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
        tone,
        options: Arc::default(),
        visibility_expression: and_gate(gates.visible.as_deref(), visibility),
        enable_expression: and_gate(gates.enabled.as_deref(), enabled),
        gates: GateState::default(),
        help: None,
        minimum: None,
        maximum: None,
        index: None,
        hex: false,
        flip: false,
        power_cycle: false,
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
fn field_row(walk: &mut RowWalk<'_>, row: &DialogRow, section: &str) -> Option<DialogField> {
    let project = walk.project;

    // `radio = orientation, "Label", bitConst, …`: the orientation is not the
    // label and the label is not the variable.
    let label_at = usize::from(row.kind == RowKind::Radio);

    let raw_label = row
        .fields
        .get(label_at)
        .map(|field| oxtuner_ini::fields::unquote(field).to_owned())
        .unwrap_or_default();
    let (label, label_tone) = split_label(&raw_label);

    let tune = project.tune();
    let reference = resolve_reference(row, project, label_at);

    let Some(reference) = reference else {
        return note_field(project, row, &raw_label, section, &walk.gates);
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
    // `EditScalar` carries only a variable name, so an array element can be
    // shown but not stored yet.
    let writable =
        !display_only && !project.is_read_only() && tune.is_writable(variable) && value.is_some();
    let power_cycle = project
        .definition()
        .extensions
        .requires_power_cycle(variable);

    let (visibility, enabled) = row_conditions(project, row, Some(reference.field));
    let (options, help) = walk.presentation(variable);

    // A full-row label may point at a blank bits constant whose options are
    // all empty (Speeduino does this). Drawing its raw value beside the text
    // would read as a value the definition never meant to show.
    if display_only
        && !options.is_empty()
        && options.iter().all(|option| option.label.trim().is_empty())
    {
        return Some(note(
            label,
            section,
            label_tone,
            visibility,
            enabled,
            &walk.gates,
        ));
    }

    let kind = match row.kind {
        RowKind::Checkbox | RowKind::Radio => DialogFieldKind::Toggle,
        RowKind::Slider => DialogFieldKind::Slider,
        _ if !options.is_empty() => DialogFieldKind::Select,
        _ => DialogFieldKind::Value,
    };

    Some(DialogField {
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
        tone: label_tone,
        options,
        visibility_expression: and_gate(walk.gates.visible.as_deref(), visibility),
        enable_expression: and_gate(walk.gates.enabled.as_deref(), enabled),
        gates: GateState::default(),
        help,
        minimum: spec.minimum,
        maximum: spec.maximum,
        index: reference.index,
        hex: has_hex_flag(row),
        flip: has_flip_flag(row),
        power_cycle,
    })
}

/// Whether a checkbox row asks for its checked state on raw zero.
///
/// `checkbox = "Label", bitConstant, flipValue[, {enable}, {visible}]`: the
/// flag sits directly after the constant, before any gate.
fn has_flip_flag(row: &DialogRow) -> bool {
    row.kind == RowKind::Checkbox
        && row.fields.get(2).is_some_and(|field| {
            let (token, trailing) = split_gate(field);

            trailing.is_none() && token.eq_ignore_ascii_case("flipvalue")
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
