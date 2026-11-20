//! `[LoggerDefinition]` and the named configuration sections.
//!
//! A logger has a declared record layout with bit offsets, so it is typed. The
//! other sections arrived later and have no consumer yet, so their entries are
//! kept as typed key/field/line records: quote- and brace-aware splitting has
//! already run, and nothing is lost to a verbatim raw line.

use crate::fields::{split_assignment, split_fields, unquote};
use crate::model::{
    ConfigEntry, ConfigSection, Definition, LoggerDefinition, LoggerField, Number, Text,
};

use crate::preprocess::SourcePos;

use super::{SectionParser, SectionSink};

/// `[LoggerDefinition]`: named loggers with a bit-level record layout. The open
/// logger and the seen-name set survive a repeated section header.
#[derive(Default)]
pub(crate) struct LoggersSink {
    current: Option<LoggerDefinition>,
    names: std::collections::BTreeSet<String>,
}

impl SectionSink for LoggersSink {
    fn feed(
        &mut self,
        definition: &mut Definition,
        section: &mut SectionParser,
        lines: &[(SourcePos, &str)],
        _expander: &crate::preprocess::Expander<'_>,
    ) {
        for (line, text) in lines {
            section.count_entry();
            let Some((key, value)) = split_assignment(text) else {
                section.unrecognized_line(*line, text);
                continue;
            };
            let fields = split_fields(value);
            let lowered = key.to_ascii_lowercase();

            if lowered == "loggerdef" {
                if let Some(logger) = self.current.take() {
                    definition.loggers.push(logger);
                }
                let opened = open_logger(&fields, *line, section, &mut self.names);
                if opened.is_none() {
                    section.warn("bad-logger", "`loggerDef` needs a name", *line);
                }
                self.current = opened;
                section.understand(*line);
                continue;
            }

            let Some(logger) = self.current.as_mut() else {
                section.warn(
                    "orphan-logger-key",
                    format!("`{key}` appears before any `loggerDef` entry"),
                    *line,
                );
                continue;
            };

            match lowered.as_str() {
                "startcommand" => logger.start_command = Some(unquote(value).to_owned()),
                "stopcommand" => logger.stop_command = Some(unquote(value).to_owned()),
                "datareadcommand" => logger.data_read_command = Some(unquote(value).to_owned()),
                "datareadtimeout" => logger.data_read_timeout_ms = crate::fields::parse_u32(value),
                "continuousread" => logger.continuous_read = crate::fields::is_true(value),
                "datareadycondition" => logger.data_ready_condition = Some(value.to_owned()),
                "datalength" => logger.data_length = crate::fields::parse_u32(value),
                "recorddef" => {
                    logger.header_len = fields
                        .first()
                        .and_then(|field| crate::fields::parse_u32(field))
                        .unwrap_or(0);
                    logger.footer_len = fields
                        .get(1)
                        .and_then(|field| crate::fields::parse_u32(field))
                        .unwrap_or(0);
                    logger.record_len = fields
                        .get(2)
                        .and_then(|field| crate::fields::parse_u32(field))
                        .unwrap_or(0);
                }
                "recordfield" => logger.fields.push(record_field(&fields, line.line)),
                "headerfield" => logger.header_fields.push(record_field(&fields, line.line)),
                "calcfield" => logger.calculated.push(calculated_field(&fields, line.line)),
                _ => {
                    section.unrecognized_key(key, *line);
                    continue;
                }
            }
            section.understand(*line);
        }
    }

    fn finish(&mut self, definition: &mut Definition, _section: &mut SectionParser) {
        if let Some(logger) = self.current.take() {
            definition.loggers.push(logger);
        }
    }
}

/// Opens one `loggerDef = name, "Title", type` block.
fn open_logger(
    fields: &[&str],
    line: SourcePos,
    section: &mut SectionParser,
    names: &mut std::collections::BTreeSet<String>,
) -> Option<LoggerDefinition> {
    let name = fields.first().copied()?;
    if !names.insert(name.to_owned()) {
        section.note(
            "duplicate-logger",
            format!("logger `{name}` is declared more than once; both records are kept"),
            line,
        );
    }
    Some(LoggerDefinition {
        name: name.to_owned(),
        title: fields.get(1).map(|field| unquote(field).to_owned()),
        kind: fields
            .get(2)
            .map_or_else(String::new, |field| field.to_ascii_lowercase()),
        start_command: None,
        stop_command: None,
        data_read_command: None,
        data_read_timeout_ms: None,
        continuous_read: false,
        data_ready_condition: None,
        data_length: None,
        header_len: 0,
        footer_len: 0,
        record_len: 0,
        fields: Vec::new(),
        header_fields: Vec::new(),
        calculated: Vec::new(),
        line: line.line,
    })
}

fn record_field(fields: &[&str], line: u32) -> LoggerField {
    let hidden = fields
        .iter()
        .any(|field| field.eq_ignore_ascii_case("hidden"));
    LoggerField {
        name: fields
            .first()
            .map_or_else(String::new, |field| (*field).to_owned()),
        header: fields.get(1).map(|field| unquote(field).to_owned()),
        start_bit: fields
            .get(2)
            .and_then(|field| crate::fields::parse_u32(field)),
        bit_count: fields
            .get(3)
            .and_then(|field| crate::fields::parse_u32(field)),
        scale: fields.get(4).map(|field| Number::parse(field)),
        units: fields
            .get(5)
            .map_or(Text::empty(), |field| Text::parse(field)),
        update_condition: fields.get(6).map(|field| (*field).to_owned()),
        expression: None,
        hidden,
        line,
    }
}

fn calculated_field(fields: &[&str], line: u32) -> LoggerField {
    let hidden = fields
        .iter()
        .any(|field| field.eq_ignore_ascii_case("hidden"));
    LoggerField {
        name: fields
            .first()
            .map_or_else(String::new, |field| (*field).to_owned()),
        header: fields.get(1).map(|field| unquote(field).to_owned()),
        start_bit: None,
        bit_count: None,
        scale: None,
        units: fields
            .get(2)
            .map_or(Text::empty(), |field| Text::parse(field)),
        update_condition: None,
        expression: fields.get(3).map(|field| (*field).to_owned()),
        hidden,
        line,
    }
}

/// A named section whose schema a later consumer will own. A repeated header
/// appends to the same entry buffer so only one `ConfigSection` is pushed at
/// the end of the document.
pub(crate) struct ConfigSectionSink {
    name: String,
    entries: Vec<ConfigEntry>,
}

impl ConfigSectionSink {
    pub(crate) fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            entries: Vec::new(),
        }
    }
}

impl SectionSink for ConfigSectionSink {
    fn feed(
        &mut self,
        _definition: &mut Definition,
        section: &mut SectionParser,
        lines: &[(SourcePos, &str)],
        _expander: &crate::preprocess::Expander<'_>,
    ) {
        for (line, text) in lines {
            section.count_entry();
            let Some((key, value)) = split_assignment(text) else {
                section.unrecognized_line(*line, text);
                continue;
            };
            self.entries.push(ConfigEntry {
                key: key.to_owned(),
                value: value.to_owned(),
                fields: split_fields(value).into_iter().map(str::to_owned).collect(),
                line: line.line,
            });
            section.understand(*line);
        }
    }

    fn finish(&mut self, definition: &mut Definition, _section: &mut SectionParser) {
        definition.config_sections.push(ConfigSection {
            name: self.name.clone(),
            entries: std::mem::take(&mut self.entries),
        });
    }
}
