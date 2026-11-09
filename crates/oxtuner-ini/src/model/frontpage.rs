use crate::expr::Expr;

use super::{Number, Text};

/// One gauge template from `[GaugeConfigurations]`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Gauge {
    /// Category the template belongs to.
    pub category: Option<String>,
    /// Template name.
    pub name: String,
    /// Channel the gauge shows.
    pub channel: String,
    /// Display title.
    pub title: Text,
    /// Display units.
    pub units: Text,
    /// Lowest value.
    pub low: Number,
    /// Highest value.
    pub high: Number,
    /// Low danger threshold.
    pub low_danger: Number,
    /// Low warning threshold.
    pub low_warning: Number,
    /// High warning threshold.
    pub high_warning: Number,
    /// High danger threshold.
    pub high_danger: Number,
    /// Value digits.
    pub value_digits: Number,
    /// Label digits.
    pub label_digits: Number,
    /// Optional availability expression.
    pub active: Option<Expr>,
    /// Source line.
    pub line: u32,
    /// Availability expression source, as written.
    ///
    /// `Expr` keeps no source text, so the parser stores it beside the parsed
    /// form for summaries that carry gates as data.
    pub active_source: Option<String>,
}

/// One dashboard indicator.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Indicator {
    /// Channel (or expression source) that drives it.
    pub channel: String,
    /// Text when off.
    pub off_label: String,
    /// Text when on.
    pub on_label: String,
    /// Remaining colour fields, as written.
    pub colours: Vec<String>,
    /// Whether this is a template-only entry.
    pub template_only: bool,
    /// Source line.
    pub line: u32,
}

/// `[FrontPage]`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct FrontPage {
    /// Gauges in slot order.
    pub gauges: Vec<(u8, String)>,
    /// Indicators.
    pub indicators: Vec<Indicator>,
}

impl FrontPage {
    /// Gauges in slot order.
    pub fn slots(&self) -> &[(u8, String)] {
        &self.gauges
    }
}
