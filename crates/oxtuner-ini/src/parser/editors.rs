//! `[TableEditor]` and `[CurveEditor]`.
//!
//! Both are block-structured: a `table =` / `curve =` line opens a block and the
//! indented keys that follow belong to it until the next one. The block form is
//! what makes `xBins`, `yBins` and `zBins` available to the table grid — a
//! table without all three cannot be laid out.

use crate::fields::split_fields;
use crate::model::{Axis, Curve, Definition, Number, Table, Text};

use super::SectionParser;

/// `[TableEditor]`.
pub(crate) fn parse_tables(
    definition: &mut Definition,
    section: &mut SectionParser,
    lines: &[(u32, &str)],
) {
    let mut current: Option<Table> = None;

    for (line, text) in lines {
        section.count_entry();
        let Some((key, value)) = crate::fields::split_assignment(text) else {
            section.unrecognized_line(*line, text);
            continue;
        };
        let lowered = key.to_ascii_lowercase();
        let fields = split_fields(value);

        if lowered == "table" {
            if let Some(table) = current.take() {
                insert_table(definition, section, table);
            }
            section.understand(*line);

            if fields.len() < 3 {
                section.warn(
                    "bad-table",
                    format!("`table = {value}` needs an id, a map id and a title"),
                    *line,
                );
                continue;
            }
            // Speeduino appends the page as a fourth field; rusEFI does not.
            let page = fields
                .get(3)
                .and_then(|field| crate::fields::parse_u32(field))
                .and_then(|page| u8::try_from(page).ok());

            current = Some(Table {
                id: fields[0].to_owned(),
                map_id: Some(fields[1].to_owned()),
                title: Text::parse(fields[2]),
                page,
                x_bins: None,
                y_bins: None,
                z_bins: None,
                xy_labels: Vec::new(),
                up_down_label: Vec::new(),
                grid_height: None,
                grid_orient: None,
                topic_help: None,
                line: *line,
            });
            continue;
        }

        let Some(table) = current.as_mut() else {
            section.warn(
                "orphan-key",
                format!("`{key}` appears before any `table =` entry"),
                *line,
            );
            continue;
        };

        match lowered.as_str() {
            "xbins" => table.x_bins = Some(parse_axis(&fields)),
            "ybins" => table.y_bins = Some(parse_axis(&fields)),
            "zbins" => {
                table.z_bins = fields.first().map(|field| (*field).to_owned());
            }
            "xylabels" => table.xy_labels = fields.iter().map(|field| Text::parse(field)).collect(),
            "updownlabel" => {
                table.up_down_label = fields.iter().map(|field| Text::parse(field)).collect();
            }
            "gridheight" => {
                table.grid_height = fields.first().map(|field| Number::parse(field));
            }
            "gridorient" => {
                let values: Vec<Number> = fields.iter().map(|field| Number::parse(field)).collect();
                if values.len() == 3 {
                    table.grid_orient =
                        Some([values[0].clone(), values[1].clone(), values[2].clone()]);
                } else {
                    section.warn(
                        "bad-grid-orient",
                        format!("`gridOrient` needs three angles, got {}", values.len()),
                        *line,
                    );
                }
            }
            "topichelp" => {
                table.topic_help = fields
                    .first()
                    .map(|field| crate::fields::unquote(field).to_owned());
            }
            // Recognised but not modelled on the table itself: window
            // protection, which belongs to the UI layer.
            "userpassword" => {
                definition
                    .comms
                    .raw
                    .insert(format!("table.{}.userpassword", table.id), value.to_owned());
            }
            _ => {
                section.unrecognized_key(key, *line);
                continue;
            }
        }
        section.understand(*line);
    }

    if let Some(table) = current {
        insert_table(definition, section, table);
    }
}

fn insert_table(definition: &mut Definition, section: &mut SectionParser, table: Table) {
    if table.z_bins.is_none() {
        section.warn(
            "table-without-cells",
            format!("table `{}` has no `zBins` cell array", table.id),
            table.line,
        );
    }
    let id = table.id.clone();
    let line = table.line;
    if let Some(existing) = definition.tables.get_mut(&id) {
        merge_table(existing, table);
        section.note(
            "merged-table",
            format!("table `{id}` is declared more than once; its fields are merged"),
            line,
        );
    } else {
        definition.tables.insert(&id, table);
    }
}

/// Applies a second `table = id` header to the first declaration without
/// dropping either block's axes or labels.
fn merge_table(existing: &mut Table, next: Table) {
    let Table {
        id: _,
        map_id,
        title,
        page,
        x_bins,
        y_bins,
        z_bins,
        xy_labels,
        up_down_label,
        grid_height,
        grid_orient,
        topic_help,
        line: _,
    } = next;

    if map_id.is_some() {
        existing.map_id = map_id;
    }
    existing.title = title;
    if page.is_some() {
        existing.page = page;
    }
    if x_bins.is_some() {
        existing.x_bins = x_bins;
    }
    if y_bins.is_some() {
        existing.y_bins = y_bins;
    }
    if z_bins.is_some() {
        existing.z_bins = z_bins;
    }
    existing.xy_labels.extend(xy_labels);
    existing.up_down_label.extend(up_down_label);
    if grid_height.is_some() {
        existing.grid_height = grid_height;
    }
    if grid_orient.is_some() {
        existing.grid_orient = grid_orient;
    }
    if topic_help.is_some() {
        existing.topic_help = topic_help;
    }
}

/// `xBins = array[, trackingChannel][, readOnly]`.
fn parse_axis(fields: &[&str]) -> Axis {
    let array = fields
        .first()
        .map_or_else(String::new, |field| (*field).to_owned());
    let channel = fields
        .get(1)
        .map(|field| crate::fields::unquote(field).to_owned());
    let read_only = fields
        .iter()
        .skip(1)
        .any(|field| field.eq_ignore_ascii_case("readOnly"));

    Axis {
        array,
        channel: channel.filter(|channel| !channel.eq_ignore_ascii_case("readOnly")),
        read_only,
    }
}

/// `[CurveEditor]`.
pub(crate) fn parse_curves(
    definition: &mut Definition,
    section: &mut SectionParser,
    lines: &[(u32, &str)],
) {
    let mut current: Option<Curve> = None;

    for (line, text) in lines {
        section.count_entry();
        let Some((key, value)) = crate::fields::split_assignment(text) else {
            section.unrecognized_line(*line, text);
            continue;
        };
        let lowered = key.to_ascii_lowercase();
        let fields = split_fields(value);

        if lowered == "curve" {
            if let Some(curve) = current.take() {
                insert_curve(definition, section, curve);
            }
            section.understand(*line);
            let Some(name) = fields.first().copied() else {
                section.warn("bad-curve", "`curve =` needs a name", *line);
                continue;
            };
            current = Some(Curve {
                name: name.to_owned(),
                title: fields
                    .get(1)
                    .map(|field| Text::parse(field))
                    .unwrap_or(Text::Literal(name.to_owned())),
                column_labels: Vec::new(),
                x_axis: Vec::new(),
                y_axis: Vec::new(),
                x_bins: None,
                y_bins: Vec::new(),
                line_labels: Vec::new(),
                gauge: None,
                show_text_values: false,
                suppress_graph: false,
                line: *line,
            });
            continue;
        }

        let Some(curve) = current.as_mut() else {
            section.warn(
                "orphan-key",
                format!("`{key}` appears before any `curve =` entry"),
                *line,
            );
            continue;
        };

        match lowered.as_str() {
            "columnlabel" => {
                curve.column_labels = fields.iter().map(|field| Text::parse(field)).collect();
            }
            "xaxis" => curve.x_axis = fields.iter().map(|field| Number::parse(field)).collect(),
            "yaxis" => curve.y_axis = fields.iter().map(|field| Number::parse(field)).collect(),
            // A curve may plot several Y arrays; order matters for the colours.
            "ybins" => curve.y_bins.push(parse_axis(&fields)),
            "xbins" => curve.x_bins = Some(parse_axis(&fields)),
            "gauge" => {
                curve.gauge = fields
                    .first()
                    .map(|field| crate::fields::unquote(field).to_owned());
            }
            "showtextvalues" => curve.show_text_values = is_true(value),
            "suppressgraph" => curve.suppress_graph = is_true(value),
            "linelabel" => curve.line_labels.push(
                fields
                    .first()
                    .map_or(Text::Literal(String::new()), |field| Text::parse(field)),
            ),
            // Presentation keys the curve editor owns: window size and help
            // topic. Kept verbatim rather than dropped.
            "size" | "topichelp" | "userpassword" => {
                definition
                    .comms
                    .raw
                    .insert(format!("curve.{}.{key}", curve.name), value.to_owned());
            }
            _ => {
                section.unrecognized_key(key, *line);
                continue;
            }
        }
        section.understand(*line);
    }

    if let Some(curve) = current {
        insert_curve(definition, section, curve);
    }
}

fn insert_curve(definition: &mut Definition, section: &mut SectionParser, curve: Curve) {
    if curve.x_bins.is_none() || curve.y_bins.is_empty() {
        section.warn(
            "curve-without-arrays",
            format!(
                "curve `{}` needs an X array and at least one Y array",
                curve.name
            ),
            curve.line,
        );
    }
    let name = curve.name.clone();
    let line = curve.line;
    if let Some(existing) = definition.curves.get_mut(&name) {
        merge_curve(existing, curve);
        section.note(
            "merged-curve",
            format!("curve `{name}` is declared more than once; its fields are merged"),
            line,
        );
    } else {
        definition.curves.insert(&name, curve);
    }
}

/// Applies a second `curve = name` header to the first declaration. Series and
/// labels accumulate; a header with a fresh range replaces the old range.
fn merge_curve(existing: &mut Curve, next: Curve) {
    let Curve {
        name: _,
        title,
        column_labels,
        x_axis,
        y_axis,
        x_bins,
        y_bins,
        line_labels,
        gauge,
        show_text_values,
        suppress_graph,
        line: _,
    } = next;

    existing.title = title;
    if !column_labels.is_empty() {
        existing.column_labels = column_labels;
    }
    if !x_axis.is_empty() {
        existing.x_axis = x_axis;
    }
    if !y_axis.is_empty() {
        existing.y_axis = y_axis;
    }
    if x_bins.is_some() {
        existing.x_bins = x_bins;
    }
    existing.y_bins.extend(y_bins);
    existing.line_labels.extend(line_labels);
    if gauge.is_some() {
        existing.gauge = gauge;
    }
    existing.show_text_values |= show_text_values;
    existing.suppress_graph |= suppress_graph;
}

fn is_true(value: &str) -> bool {
    matches!(
        value.trim_ascii().to_ascii_lowercase().as_str(),
        "true" | "on" | "1" | "yes"
    )
}
