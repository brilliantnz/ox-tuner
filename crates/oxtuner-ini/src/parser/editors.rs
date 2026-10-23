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
                id: fields[0].clone(),
                map_id: Some(fields[1].clone()),
                title: Text::parse(&fields[2]),
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
                table.z_bins = fields.first().cloned();
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
                    .protocol
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
    if definition.tables.insert(&id, table).is_some() {
        section.error(
            "duplicate-name",
            format!("table `{id}` is declared more than once; the later wins"),
            0,
        );
    }
}

/// `xBins = array[, trackingChannel][, readOnly]`.
fn parse_axis(fields: &[String]) -> Axis {
    let array = fields.first().cloned().unwrap_or_default();
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
            let Some(name) = fields.first() else {
                section.warn("bad-curve", "`curve =` needs a name", *line);
                continue;
            };
            current = Some(Curve {
                name: name.clone(),
                title: fields
                    .get(1)
                    .map(|field| Text::parse(field))
                    .unwrap_or(Text::Literal(name.clone())),
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
                    .map(|field| Text::parse(field))
                    .unwrap_or(Text::Literal(String::new())),
            ),
            // Presentation keys the curve editor owns: window size and help
            // topic. Kept verbatim rather than dropped.
            "size" | "topichelp" | "userpassword" => {
                definition
                    .protocol
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
    if definition.curves.insert(&name, curve).is_some() {
        section.error(
            "duplicate-name",
            format!("curve `{name}` is declared more than once; the later wins"),
            0,
        );
    }
}

fn is_true(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "on" | "1" | "yes"
    )
}
