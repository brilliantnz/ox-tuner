//! CSV shape: one column per realtime channel plus a time column.

use std::fmt::Write as _;

/// One column of a session log.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogColumn {
    /// Channel name as the definition writes it.
    pub name: String,
    /// Display units, for the log header comments the read side can show.
    pub units: String,
}

impl LogColumn {
    /// A channel column.
    pub fn new(name: impl Into<String>, units: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            units: units.into(),
        }
    }
}

/// The header row, ending in a newline.
pub(crate) fn header(columns: &[LogColumn]) -> String {
    let mut line = String::from("Time");
    for column in columns {
        line.push(',');
        line.push_str(&escape(&column.name));
    }
    line.push('\n');
    line
}

/// One data row, ending in a newline.
///
/// Values outside the column list are ignored; missing values are written as
/// empty cells so a spreadsheet sees a gap rather than a zero.
pub(crate) fn row(time: f64, columns: &[LogColumn], values: &[f64]) -> String {
    let mut line = String::with_capacity(16 + columns.len() * 10);
    let _ = write!(line, "{time:.4}");
    for index in 0..columns.len() {
        line.push(',');
        if let Some(value) = values.get(index)
            && value.is_finite()
        {
            let _ = write!(line, "{value:.4}");
        }
    }
    line.push('\n');
    line
}

/// CSV-safe field text.
///
/// Channel names are definition data; a comma or quote in one must not break
/// the column count. This is the standard double-quote escaping.
fn escape(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_lists_time_then_every_channel() {
        let columns = [LogColumn::new("rpm", "rpm"), LogColumn::new("map", "kPa")];

        assert_eq!(header(&columns), "Time,rpm,map\n");
    }

    #[test]
    fn a_name_with_a_comma_is_quoted() {
        let columns = [LogColumn::new("a,b", "")];

        assert_eq!(header(&columns), "Time,\"a,b\"\n");
    }

    #[test]
    fn a_row_formats_four_decimals_and_leaves_gaps_empty() {
        let columns = [
            LogColumn::new("rpm", "rpm"),
            LogColumn::new("map", "kPa"),
            LogColumn::new("afr", ""),
        ];

        let line = row(1.23456, &columns, &[3500.0, f64::NAN]);

        assert_eq!(line, "1.2346,3500.0000,,\n");
    }
}
