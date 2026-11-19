//! Cross-model checks that stream over the frozen definition once.
//!
//! Each check emits at most one note per distinct call name, so a file that
//! repeats a runtime helper on every line does not flood the diagnostics. A
//! note never changes the model: a call this build cannot fold stays exactly as
//! it was written.

use std::collections::{BTreeMap, HashSet};

use crate::error::{Diagnostic, Severity, extend_bounded};
use crate::expr::Expr;
use crate::model::{ChannelKind, Definition, Dimension, Number, Shape, Text};

/// A reason to keep a call symbolic, keyed by its lowercase name.
type Issues = BTreeMap<String, (u32, String, String)>;

/// Records each view's live cursor once, from the axis that carries a tracking
/// channel. The first `yBins` with a channel wins for multi-series curves.
pub(crate) fn promote_live_cursors(definition: &mut Definition) {
    for (_, table) in definition.tables.iter_mut() {
        if table.live_cursor.is_none() {
            table.live_cursor = table
                .x_bins
                .as_ref()
                .and_then(|axis| axis.channel.clone())
                .or_else(|| table.y_bins.as_ref().and_then(|axis| axis.channel.clone()));
        }
    }

    for (_, curve) in definition.curves.iter_mut() {
        if curve.live_cursor.is_none() {
            curve.live_cursor = curve
                .y_bins
                .iter()
                .find_map(|axis| axis.channel.clone())
                .or_else(|| curve.x_bins.as_ref().and_then(|axis| axis.channel.clone()));
        }
    }
}

pub(crate) fn validate_expression_calls(definition: &mut Definition) {
    let mut issues = Issues::new();

    walk_entries(definition, &mut issues);
    walk_editors(definition, &mut issues);
    walk_dashboard(definition, &mut issues);
    walk_dialogs(definition, &mut issues);

    let mut seen = HashSet::new();
    let diagnostics: Vec<Diagnostic> = issues
        .into_values()
        .filter(|(line, section, reason)| seen.insert((*line, section.clone(), reason.clone())))
        .map(|(line, section, reason)| {
            Diagnostic::new(Severity::Note, "fold-deferred", reason, line, &section)
        })
        .collect();
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
                if let Some(body) = crate::fields::is_expression_field(field)
                    .then(|| crate::fields::expression_body(field))
                    .flatten()
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
                let trimmed = condition.trim_ascii();
                let body = trimmed
                    .strip_prefix('{')
                    .and_then(|text| text.strip_suffix('}'))
                    .unwrap_or(trimmed);
                if let Ok(expression) = Expr::parse(body) {
                    add_expr(&expression, item.line, "Menu", issues);
                }
            }
        }
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
        issues
            .entry(name.to_ascii_lowercase())
            .or_insert((line, section.to_owned(), reason));
    }
}
