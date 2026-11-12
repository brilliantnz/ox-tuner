//! `[ConstantsExtensions]`, `[ControllerCommands]`, `[ReferenceTables]` and
//! `[Datalog]`.
//!
//! These sections are parsed into the model but rendered late (v1 knows what
//! they mean; the UI that uses them arrives with the features they serve). They
//! are still first-class data — a definition that carries them must not lose
//! them.

use std::collections::BTreeMap;

use crate::fields::{CommandTemplate, split_fields};
use crate::model::{ControllerCommand, DatalogEntry, Definition, Number, ReferenceTable};

use super::SectionParser;

/// `[ConstantsExtensions]` (rusEFI spells it in the plural) and `[Defaults]`.
pub(crate) fn parse_extensions(
    definition: &mut Definition,
    section: &mut SectionParser,
    lines: &[(u32, &str)],
) {
    for (line, text) in lines {
        let Some((key, value)) = super::entry_line(section, *line, text) else {
            continue;
        };
        let (name, rest) = split_reference(value);

        match key.to_ascii_lowercase().as_str() {
            "defaultvalue" | "defaults" => {
                if let Some(rest) = rest {
                    definition
                        .extensions
                        .defaults
                        .insert(&name, Number::parse(&rest));
                    section.understand(*line);
                } else {
                    section.warn(
                        "bad-default",
                        format!("`{key} = {value}` needs a name and a value"),
                        *line,
                    );
                }
            }
            "rawvalue" => {
                if let Some(rest) = rest {
                    definition
                        .extensions
                        .raw_defaults
                        .insert(&name, Number::parse(&rest));
                    section.understand(*line);
                }
            }
            "maximumelements" => {
                if let Some(rest) = rest
                    && let Some(value) = crate::fields::parse_u32(&rest)
                {
                    definition.extensions.maximum_elements.insert(&name, value);
                    section.understand(*line);
                }
            }
            "readonly" => {
                definition.extensions.read_only.insert(name);
                section.understand(*line);
            }
            "requirespowercycle" => {
                definition.extensions.power_cycle.insert(name);
                section.understand(*line);
            }
            "controllerpriority" => {
                definition.extensions.controller_priority.insert(name);
                section.understand(*line);
            }
            "reverseindex" => {
                definition.extensions.reverse_index.insert(name);
                section.understand(*line);
            }
            "oppositeendian" => {
                definition.extensions.opposite_endian.insert(name);
                section.understand(*line);
            }
            "usemappingtable" => {
                definition
                    .extensions
                    .mapping_tables
                    .insert(&name, rest.unwrap_or_default());
                section.understand(*line);
            }
            "maintainconstantvalue" => {
                definition
                    .extensions
                    .maintain
                    .push((name, rest.unwrap_or_default()));
                section.understand(*line);
            }
            "ignoremissingbitoptions" => {
                definition.extensions.ignore_missing_bit_options = is_true(value);
                section.understand(*line);
            }
            // The dialect's own key contains the typo; accept both spellings.
            "surpressconfigerrorverbiage" | "suppressconfigerrorverbiage" => {
                definition.extensions.suppress_config_error_verbiage = is_true(value);
                section.understand(*line);
            }
            _ => section.unrecognized_key(key, *line),
        }
    }
}

/// Splits `name, value` (or `name value`) into its two halves.
fn split_reference(value: &str) -> (String, Option<String>) {
    let mut fields = split_fields(value);
    if fields.len() >= 2 {
        let name = fields.remove(0).to_owned();
        return (name, Some(fields.join(", ")));
    }
    match value.split_once(char::is_whitespace) {
        Some((name, rest)) if !rest.trim_ascii().is_empty() => (
            name.trim_ascii().to_owned(),
            Some(rest.trim_ascii().to_owned()),
        ),
        _ => (value.trim_ascii().to_owned(), None),
    }
}

fn is_true(value: &str) -> bool {
    matches!(
        value.trim_ascii().to_ascii_lowercase().as_str(),
        "true" | "on" | "1" | "yes"
    )
}

/// `[ControllerCommands]`.
pub(crate) fn parse_controller_commands(
    definition: &mut Definition,
    section: &mut SectionParser,
    lines: &[(u32, &str)],
) {
    for (line, text) in lines {
        let Some((key, value)) = super::entry_line(section, *line, text) else {
            continue;
        };

        if key.eq_ignore_ascii_case("displayCommand") {
            let fields = split_fields(value);
            if let Some(name) = fields.first()
                && let Some(command) = definition.controller_commands.get_mut(name)
            {
                command.description = Some(
                    fields
                        .get(1)
                        .map(|field| crate::fields::unquote(field).to_owned())
                        .unwrap_or_default(),
                );
            }
            section.understand(*line);
            continue;
        }

        let commands: Vec<CommandTemplate> = split_fields(value)
            .iter()
            .map(|field| CommandTemplate::parse(field))
            .collect();
        if commands.is_empty() {
            section.warn(
                "bad-controller-command",
                format!("`{key}` has no command string"),
                *line,
            );
            continue;
        }

        definition.controller_commands.insert(
            key,
            ControllerCommand {
                name: key.to_owned(),
                commands,
                description: None,
                line: *line,
            },
        );
        section.understand(*line);
    }
}

/// `[ReferenceTables]`.
pub(crate) fn parse_reference_tables(
    definition: &mut Definition,
    section: &mut SectionParser,
    lines: &[(u32, &str)],
) {
    let mut current: Option<String> = None;

    for (line, text) in lines {
        let Some((key, value)) = super::entry_line(section, *line, text) else {
            continue;
        };
        let lowered = key.to_ascii_lowercase();
        let fields = split_fields(value);

        if lowered == "referencetable" {
            let Some(name) = fields.first().copied() else {
                section.warn(
                    "bad-reference-table",
                    "`referenceTable =` needs a name",
                    *line,
                );
                continue;
            };
            definition.reference_tables.insert(
                name,
                ReferenceTable {
                    name: name.to_owned(),
                    title: fields
                        .get(1)
                        .map(|field| crate::fields::unquote(field).to_owned()),
                    line: *line,
                    ..ReferenceTable::default()
                },
            );
            current = Some(name.to_owned());
            section.understand(*line);
            continue;
        }

        // Section-level commands.
        if matches!(
            lowered.as_str(),
            "tablewritecommand" | "tableblockingfactor" | "tablecrccommand"
        ) {
            definition.comms.raw.insert(lowered, value.to_owned());
            section.understand(*line);
            continue;
        }

        let Some(name) = current.clone() else {
            section.warn(
                "orphan-reference-table-key",
                format!("`{key}` appears before any `referenceTable =` entry"),
                *line,
            );
            continue;
        };
        let Some(table) = definition.reference_tables.get_mut(&name) else {
            continue;
        };

        let understood = apply_reference_table_key(
            table,
            &mut definition.comms.raw,
            &lowered,
            &fields,
            &name,
            key,
            value,
        );
        if !understood {
            section.unrecognized_key(key, *line);
            continue;
        }
        section.understand(*line);
    }
}

/// Applies one key inside an open `[ReferenceTables]` entry.
///
/// Returns `false` when the key belongs to no known reference-table field, so
/// the caller can report it once.
fn apply_reference_table_key(
    table: &mut ReferenceTable,
    raw: &mut BTreeMap<String, String>,
    lowered: &str,
    fields: &[&str],
    name: &str,
    key: &str,
    value: &str,
) -> bool {
    match lowered {
        "tableidentifier" => {
            let mut index = 0usize;
            while index + 1 < fields.len() {
                if let Some(id) = crate::fields::parse_u32(fields[index]) {
                    table
                        .identifiers
                        .push((id, crate::fields::unquote(fields[index + 1]).to_owned()));
                }
                index += 2;
            }
        }
        "adccount" => {
            table.adc_count = fields
                .first()
                .and_then(|field| crate::fields::parse_u32(field));
        }
        "bytesperadc" => {
            table.bytes_per_adc = fields
                .first()
                .and_then(|field| crate::fields::parse_u32(field));
        }
        "scale" => {
            table.scale = fields.first().map(|field| Number::parse(field));
        }
        "tablestartoffset" => {
            table.start_offset = fields
                .first()
                .and_then(|field| crate::fields::parse_u32(field));
        }
        "tablelimits" => {
            if fields.len() >= 4
                && let Some(id) = crate::fields::parse_u32(fields[0])
            {
                table.limits.push((
                    id,
                    Number::parse(fields[1]),
                    Number::parse(fields[2]),
                    Number::parse(fields[3]),
                ));
            }
        }
        "solution" => {
            table.solutions.push((
                fields
                    .first()
                    .map(|field| crate::fields::unquote(field).to_owned())
                    .unwrap_or_default(),
                fields.get(1).map_or_else(String::new, |f| (*f).to_owned()),
            ));
        }
        // Generation helpers are kept verbatim until the calibration UI
        // arrives; they are self-contained data.
        "solutionslabel"
        | "thermgenerator"
        | "lineargenerator"
        | "filebrowsegenerator"
        | "thermoption"
        | "tablegenerator"
        | "topichelp" => {
            raw.insert(format!("referenceTable.{name}.{key}"), value.to_owned());
        }
        _ => return false,
    }
    true
}

/// `[Datalog]`.
pub(crate) fn parse_datalog(
    definition: &mut Definition,
    section: &mut SectionParser,
    lines: &[(u32, &str)],
) {
    let mut category: Option<String> = None;

    for (line, text) in lines {
        let Some((key, value)) = super::entry_line(section, *line, text) else {
            continue;
        };
        let lowered = key.to_ascii_lowercase();
        let fields = split_fields(value);

        match lowered.as_str() {
            "category" => {
                category = fields
                    .first()
                    .map(|field| crate::fields::unquote(field).to_owned());
                section.understand(*line);
            }
            "entry" => {
                if fields.len() < 4 {
                    section.warn(
                        "bad-datalog-entry",
                        format!("`entry = {value}` needs a channel, a label, a type and a format"),
                        *line,
                    );
                    continue;
                }
                definition.datalog_entries.push(DatalogEntry {
                    channel: crate::fields::unquote(fields[0]).to_owned(),
                    label: crate::fields::unquote(fields[1]).to_owned(),
                    value_type: Some(fields[2].to_owned()),
                    format: fields[3].to_owned(),
                    enable: fields.get(4).map(|f| (*f).to_owned()),
                    lag: fields.get(5).map(|f| (*f).to_owned()),
                    category: category.clone(),
                    line: *line,
                });
                section.understand(*line);
            }
            _ => section.unrecognized_key(key, *line),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SettingsContext;

    fn parse(text: &str) -> Definition {
        crate::parse_str(text, &SettingsContext::new()).expect("parses")
    }

    #[test]
    fn extension_defaults_accept_comma_and_space_separated_forms() {
        let definition = parse(
            "[MegaTune]\nsignature = \"x\"\n[ConstantsExtensions]\ndefaultValue = fuel_rows, 16\ndefaultValue = fuel_cols 12\nreadOnly = engineMake\n",
        );

        assert_eq!(
            definition
                .extensions
                .defaults
                .get("fuel_rows")
                .and_then(Number::literal),
            Some(16.0)
        );
        assert_eq!(
            definition
                .extensions
                .defaults
                .get("fuel_cols")
                .and_then(Number::literal),
            Some(12.0)
        );
        assert!(definition.extensions.is_read_only("engineMake"));
    }

    #[test]
    fn a_controller_command_keeps_every_command_in_order() {
        let definition = parse(
            "[MegaTune]\nsignature = \"x\"\n[ControllerCommands]\ncmdTest = \"E\\x01\\x01\", \"E\\x02\"\n",
        );

        let command = definition
            .controller_commands
            .get("cmdTest")
            .expect("command");
        assert_eq!(command.commands.len(), 2);
    }

    #[test]
    fn a_reference_table_collects_identifiers_and_limits() {
        let definition = parse(
            "[MegaTune]\nsignature = \"x\"\n[ReferenceTables]\nreferenceTable = clt, \"CLT\"\ntableIdentifier = 1, \"CLT\"\nadcCount = 1024\nbytesPerAdc = 2\nscale = 10\ntableLimits = 1, 0, 100, 25\n",
        );

        let table = definition.reference_tables.get("clt").expect("table");
        assert_eq!(table.adc_count, Some(1024));
        assert_eq!(table.bytes_per_adc, Some(2));
        assert_eq!(table.identifiers, vec![(1, "CLT".to_owned())]);
        assert_eq!(table.limits.len(), 1);
    }

    #[test]
    fn reference_table_commands_share_one_lowercase_key() {
        let definition = parse(
            "[MegaTune]\nsignature = \"x\"\n[ReferenceTables]\nreferenceTable = clt, \"CLT\"\ntableWriteCommand = \"W\"\ntableBlockingFactor = 8\nsolutionsLabel = \"CLT\"\n",
        );

        assert_eq!(
            definition
                .comms
                .raw
                .get("tablewritecommand")
                .map(String::as_str),
            Some("\"W\"")
        );
        assert_eq!(
            definition
                .comms
                .raw
                .get("tableblockingfactor")
                .map(String::as_str),
            Some("8")
        );
        assert_eq!(
            definition
                .comms
                .raw
                .get("referenceTable.clt.solutionsLabel")
                .map(String::as_str),
            Some("\"CLT\""),
            "synthetic keys keep the spelling of the names they embed"
        );
    }

    #[test]
    fn a_datalog_entry_inherits_the_category_above_it() {
        let definition = parse(
            "[MegaTune]\nsignature = \"x\"\n[Datalog]\ncategory = \"Fuel\"\nentry = rpm, \"RPM\", int, %.0f\n",
        );

        assert_eq!(
            definition.datalog_entries[0].category.as_deref(),
            Some("Fuel")
        );
        assert_eq!(definition.datalog_entries[0].label, "RPM");
    }
}
