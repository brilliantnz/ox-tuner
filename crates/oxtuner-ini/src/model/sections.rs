//! Typed data for sections that arrived after the core model was frozen.
//!
//! A logger is modelled field by field because bit offsets and update
//! conditions matter to the datalog pipeline. The remaining named sections keep
//! a typed entry list: key, raw value, brace- and quote-aware fields, and the
//! source line. Nothing is dropped, and a future consumer can lift an entry
//! into a richer type without re-reading the file.

use super::{Number, Text};

/// One field of a logger record or of a derived calculation.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct LoggerField {
    /// Name used inside the logger and in expressions.
    pub name: String,
    /// Header written to the log, when present.
    pub header: Option<String>,
    /// First bit inside the record.
    pub start_bit: Option<u32>,
    /// Number of bits read from the record.
    pub bit_count: Option<u32>,
    /// Scale applied to the raw value.
    pub scale: Option<Number>,
    /// Units written to the log header.
    pub units: Text,
    /// Update condition, kept symbolic.
    pub update_condition: Option<String>,
    /// Derived fields may be hidden from the log.
    pub hidden: bool,
    /// Source line.
    pub line: u32,
}

/// One high-speed logger from `[LoggerDefinition]`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct LoggerDefinition {
    /// Stable name from `loggerDef`.
    pub name: String,
    /// Display title.
    pub title: Option<String>,
    /// Logger type written in the definition (`tooth`, `composite`, …).
    pub kind: String,
    /// Command that starts the logger.
    pub start_command: Option<String>,
    /// Command that stops the logger.
    pub stop_command: Option<String>,
    /// Command that reads the collected records.
    pub data_read_command: Option<String>,
    /// Read timeout in milliseconds.
    pub data_read_timeout_ms: Option<u32>,
    /// Whether reads continue without a stop.
    pub continuous_read: bool,
    /// Condition that says data is ready, kept symbolic.
    pub data_ready_condition: Option<String>,
    /// Declared record count or byte length.
    pub data_length: Option<u32>,
    /// Bytes in front of each record.
    pub header_len: u32,
    /// Bytes after each record.
    pub footer_len: u32,
    /// Bytes in one record.
    pub record_len: u32,
    /// Fields decoded from each record.
    pub fields: Vec<LoggerField>,
    /// Derived fields, kept opaque.
    pub calculated: Vec<LoggerField>,
    /// Source line of the `loggerDef` entry.
    pub line: u32,
}

/// One entry of a named configuration section.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct ConfigEntry {
    /// Key as written.
    pub key: String,
    /// Value after `=`, as written.
    pub value: String,
    /// Fields split quote- and brace-aware.
    pub fields: Vec<String>,
    /// Source line.
    pub line: u32,
}

/// A named section whose schema a later consumer will own.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct ConfigSection {
    /// Section header as written.
    pub name: String,
    /// Entries in document order.
    pub entries: Vec<ConfigEntry>,
}
