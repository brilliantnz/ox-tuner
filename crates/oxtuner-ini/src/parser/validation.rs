//! Cross-model checks that stream over the frozen definition once.
//!
//! Each check emits at most one note per distinct call name, so a file that
//! repeats a runtime helper on every line does not flood the diagnostics. A
//! note never changes the model: a call this build cannot fold stays exactly as
//! it was written.

use std::collections::BTreeMap;

use crate::model::Capabilities;

use crate::error::{Diagnostic, Severity, extend_bounded};
use crate::expr::Expr;
use crate::model::{ChannelKind, Definition, Dimension, Number, Shape, Text};

/// Node count above which a definition is reported as over budget. Each tree
/// already has a hard depth and source cap; this is a whole-file report, not an
/// additional allocation bound.
pub(crate) const MAX_TOTAL_AST_NODES: usize = 100_000;

/// Reasons to keep calls symbolic, plus the file's total AST node count.
struct Issues {
    reasons: BTreeMap<String, (u32, String, String)>,
    nodes: usize,
}

impl Issues {
    fn new() -> Self {
        Self {
            reasons: BTreeMap::new(),
            nodes: 0,
        }
    }
}

/// Resolves shape budgets from the constant extensions, groups tables that
/// share a dashboard id, and derives capability flags from the frozen model.
pub(crate) fn resolve_dynamic_shapes(definition: &mut Definition) {
    let Definition {
        tables,
        extensions,
        comms,
        front_page,
        ..
    } = definition;

    for (_, table) in tables.iter_mut() {
        table.x_size = table.x_bins.as_ref().and_then(|axis| {
            extensions
                .maximum_elements
                .get_ignore_case(&axis.array)
                .copied()
        });
        table.y_size = table.y_bins.as_ref().and_then(|axis| {
            extensions
                .maximum_elements
                .get_ignore_case(&axis.array)
                .copied()
        });
    }

    let mut map_ids: BTreeMap<String, usize> = BTreeMap::new();
    for (_, table) in tables.iter() {
        if let Some(id) = &table.map_id {
            *map_ids.entry(id.clone()).or_default() += 1;
        }
    }
    for (_, table) in tables.iter_mut() {
        if let Some(id) = &table.map_id
            && map_ids.get(id).copied().unwrap_or(0) > 1
        {
            table.sibling_group = Some(id.clone());
        }
    }

    definition.capabilities = Capabilities {
        has_tables: !tables.is_empty(),
        has_burst: comms.burst_get_command.is_some(),
        has_frontpage: !front_page.slots().is_empty() || !front_page.indicators.is_empty(),
    };
}

/// Records each view's live cursor once, from the axis that carries a tracking
/// channel. The first `yBins` with a channel wins for multi-series curves.
pub(crate) fn promote_live_cursors(definition: &mut Definition) {
    for (_, table) in definition.tables.iter_mut() {
        let y_channel = table.y_bins.as_ref().and_then(|axis| axis.channel.clone());
        table.live_cursor_y.clone_from(&y_channel);
        table.live_cursor = table
            .x_bins
            .as_ref()
            .and_then(|axis| axis.channel.clone())
            .or(y_channel);
    }

    for (_, curve) in definition.curves.iter_mut() {
        let y_channel = curve.y_bins.iter().find_map(|axis| axis.channel.clone());
        curve.live_cursor_y.clone_from(&y_channel);
        curve.live_cursor = curve
            .x_bins
            .as_ref()
            .and_then(|axis| axis.channel.clone())
            .or(y_channel);
    }
}

pub(crate) fn validate_expression_calls(definition: &mut Definition) {
    let mut issues = Issues::new();

    walk_entries(definition, &mut issues);
    walk_editors(definition, &mut issues);
    walk_dashboard(definition, &mut issues);
    walk_dialogs(definition, &mut issues);
    walk_loggers(definition, &mut issues);

    let mut diagnostics: Vec<Diagnostic> = issues
        .reasons
        .into_values()
        .map(|(line, section, reason)| {
            Diagnostic::new(Severity::Note, "fold-deferred", reason, line, &section)
        })
        .collect();

    if issues.nodes > MAX_TOTAL_AST_NODES {
        diagnostics.push(Diagnostic::whole_file(
            Severity::Warning,
            "ast-budget-exceeded",
            format!(
                "the definition holds {} expression nodes, over the {MAX_TOTAL_AST_NODES} budget",
                issues.nodes
            ),
        ));
    }

    extend_bounded(&mut definition.diagnostics, diagnostics);
}

/// Constants, PC variables and output channels carry the dense numeric and text
/// attributes, so they are walked together.
fn walk_entries(definition: &Definition, issues: &mut Issues) {
    for (_, constant) in definition.constants.iter() {
        let section = "Constants";
        add_optional_number(constant.scale.as_ref(), constant.line, section, issues);
        add_optional_number(constant.translate.as_ref(), constant.line, section, issues);
        add_optional_number(constant.minimum.as_ref(), constant.line, section, issues);
        add_optional_number(constant.maximum.as_ref(), constant.line, section, issues);
        add_optional_number(constant.digits.as_ref(), constant.line, section, issues);
        add_text(&constant.units, constant.line, section, issues);
        add_shape(&constant.shape, constant.line, section, issues);
    }

    for (_, variable) in definition.pc_variables.iter() {
        let section = "PcVariables";
        add_optional_number(variable.scale.as_ref(), variable.line, section, issues);
        add_optional_number(variable.translate.as_ref(), variable.line, section, issues);
        add_optional_number(variable.minimum.as_ref(), variable.line, section, issues);
        add_optional_number(variable.maximum.as_ref(), variable.line, section, issues);
        add_optional_number(variable.digits.as_ref(), variable.line, section, issues);
        add_text(&variable.units, variable.line, section, issues);
        add_shape(&variable.shape, variable.line, section, issues);
    }

    for (_, channel) in definition.channels.iter() {
        let section = "OutputChannels";
        add_number(&channel.scale, channel.line, section, issues);
        add_number(&channel.translate, channel.line, section, issues);
        add_text(&channel.units, channel.line, section, issues);
        if let ChannelKind::Computed(expression) = &channel.kind {
            add_expr(expression, channel.line, section, issues);
        }
    }
}

fn walk_editors(definition: &Definition, issues: &mut Issues) {
    for (_, table) in definition.tables.iter() {
        let section = "TableEditor";
        add_text(&table.title, table.line, section, issues);
        for label in &table.xy_labels {
            add_text(label, table.line, section, issues);
        }
        for label in &table.up_down_label {
            add_text(label, table.line, section, issues);
        }
        add_optional_number(table.grid_height.as_ref(), table.line, section, issues);
    }

    for (_, curve) in definition.curves.iter() {
        let section = "CurveEditor";
        add_text(&curve.title, curve.line, section, issues);
        for label in &curve.column_labels {
            add_text(label, curve.line, section, issues);
        }
        for label in &curve.line_labels {
            add_text(label, curve.line, section, issues);
        }
        for number in &curve.x_axis {
            add_number(number, curve.line, section, issues);
        }
        for number in &curve.y_axis {
            add_number(number, curve.line, section, issues);
        }
    }
}

fn walk_dashboard(definition: &Definition, issues: &mut Issues) {
    for gauge in &definition.gauges {
        let section = "GaugeConfigurations";
        add_text(&gauge.title, gauge.line, section, issues);
        add_text(&gauge.units, gauge.line, section, issues);
        for number in [
            &gauge.low,
            &gauge.high,
            &gauge.low_danger,
            &gauge.low_warning,
            &gauge.high_warning,
            &gauge.high_danger,
            &gauge.value_digits,
            &gauge.label_digits,
        ] {
            add_number(number, gauge.line, section, issues);
        }
        if let Some(expression) = &gauge.active {
            add_expr(expression, gauge.line, section, issues);
        }
    }

    for indicator in &definition.front_page.indicators {
        if let Ok(expression) = Expr::parse(&indicator.channel) {
            add_expr(&expression, indicator.line, "FrontPage", issues);
        }
    }
}

fn walk_dialogs(definition: &Definition, issues: &mut Issues) {
    for (_, dialog) in definition.dialogs.iter() {
        for row in &dialog.rows {
            for field in &row.fields {
                if let Some(body) = crate::fields::expression_body(field)
                    && let Ok(expression) = Expr::parse(body)
                {
                    add_expr(&expression, row.line, &dialog.name, issues);
                }
            }
        }
    }

    for menu in &definition.menus {
        for item in &menu.items {
            for condition in &item.conditions {
                add_symbolic(Some(condition), item.line, "Menu", issues);
            }
        }
    }
}

/// Logger conditions and calc expressions are symbolic strings, so they are
/// parsed here rather than carried as typed attributes.
fn walk_loggers(definition: &Definition, issues: &mut Issues) {
    for logger in &definition.loggers {
        add_symbolic(
            logger.data_ready_condition.as_deref(),
            logger.line,
            "LoggerDefinition",
            issues,
        );
        for field in logger
            .fields
            .iter()
            .chain(&logger.header_fields)
            .chain(&logger.calculated)
        {
            add_symbolic(
                field.update_condition.as_deref(),
                field.line,
                "LoggerDefinition",
                issues,
            );
            add_symbolic(
                field.expression.as_deref(),
                field.line,
                "LoggerDefinition",
                issues,
            );
        }
    }
}

fn add_symbolic(text: Option<&str>, line: u32, section: &str, issues: &mut Issues) {
    let Some(text) = text else {
        return;
    };
    let trimmed = text.trim_ascii();
    let body = trimmed
        .strip_prefix('{')
        .and_then(|inner| inner.strip_suffix('}'))
        .unwrap_or(trimmed);
    if let Ok(expression) = Expr::parse(body) {
        add_expr(&expression, line, section, issues);
    }
}

fn add_optional_number(number: Option<&Number>, line: u32, section: &str, issues: &mut Issues) {
    if let Some(number) = number {
        add_number(number, line, section, issues);
    }
}

fn add_number(number: &Number, line: u32, section: &str, issues: &mut Issues) {
    if let Number::Expression {
        parsed: Some(expression),
        ..
    } = number
    {
        add_expr(expression, line, section, issues);
    }
}

fn add_text(text: &Text, line: u32, section: &str, issues: &mut Issues) {
    if let Text::Expression {
        parsed: Some(expression),
        ..
    } = text
    {
        add_expr(expression, line, section, issues);
    }
}

fn add_shape(shape: &Shape, line: u32, section: &str, issues: &mut Issues) {
    match shape {
        Shape::Scalar | Shape::Bits { .. } => {}
        Shape::List(dimension)
        | Shape::Text {
            length: dimension, ..
        } => add_dimension(dimension, line, section, issues),
        Shape::Table { cols, rows } => {
            add_dimension(cols, line, section, issues);
            add_dimension(rows, line, section, issues);
        }
    }
}

fn add_dimension(dimension: &Dimension, line: u32, section: &str, issues: &mut Issues) {
    if let Dimension::Expression { parsed, .. } = dimension {
        add_expr(parsed, line, section, issues);
    }
}

fn add_expr(expression: &Expr, line: u32, section: &str, issues: &mut Issues) {
    issues.nodes += expression.node_count();
    for (name, arity) in expression.call_sites() {
        let reason = match crate::builtins::lookup(name) {
            None => format!("`{name}` is not a known builtin; the call is kept as data"),
            Some(builtin) if !builtin.arity.accepts(arity) => format!(
                "`{name}` takes {} argument(s) but this call passes {arity}",
                builtin.arity
            ),
            Some(builtin) if builtin.kind == crate::builtins::BuiltinKind::Deferred => {
                format!("`{name}` needs runtime state and stays a deferred call")
            }
            Some(builtin) if builtin.kind == crate::builtins::BuiltinKind::String => {
                format!("`{name}` is resolved as text at render time")
            }
            Some(_) => continue,
        };
        issues.reasons.entry(name.to_ascii_lowercase()).or_insert((
            line,
            section.to_owned(),
            reason,
        ));
    }
}
