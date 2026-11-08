//! `[FrontPage]` data: indicators, gauge slots and gauge templates.
//!
//! All three serve surfaces outside the dialog editor: the Indicators tab, the
//! dashboard and the realtime frame evaluation read this module, never
//! `editor/*`. A gauge template reference inside a dialog stays a
//! [`DialogWidget`]; the template it names lives here.

use oxtuner_ini::Gauge;

/// The load result, shaped for the UI rather than for the parser.
///
/// One `[FrontPage]` indicator as the interface sees it.
///
/// The labels are definition text and stay untranslated. The expression is the
/// source the core evaluates, kept for a future tooltip and for support.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndicatorSummary {
    /// The expression source, as the definition wrote it.
    pub expression: String,
    /// The label while the expression is zero.
    pub off_label: String,
    /// The label while the expression is non-zero.
    pub on_label: String,
    /// The definition's on background colour name, as written.
    ///
    /// The UI maps this to a semantic tone; it does not paint the name as a
    /// colour of its own.
    pub on_colour: Option<String>,
    /// The source line, for a diagnostic or a tooltip.
    pub line: u32,
}

/// One dashboard slot and the gauge template it shows.
///
/// Slots come from `[FrontPage]` (`gauge1..gaugeN = template`), in slot order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontPageGauge {
    /// Slot number as written (`gauge1` is slot 1).
    pub slot: u8,
    /// Template name from `[GaugeConfigurations]`.
    pub template: String,
}

/// One gauge template from `[GaugeConfigurations]`, as the UI draws it.
///
/// Rendering is entirely application-side: the template is data (channel,
/// limits, digits), the dial style is ours to choose. Titles and units carry
/// the literal text, or the `{source}` of a dynamic string function for a
/// future evaluator. Numeric fields carry a value only when the definition
/// states a literal; expressions and `auto` yield `None` and the app falls
/// back to witnessed data.
#[derive(Debug, Clone, PartialEq)]
pub struct GaugeSummary {
    /// Template name.
    pub name: String,
    /// Category the template belongs to, when the file groups them.
    pub category: Option<String>,
    /// Channel the gauge shows.
    pub channel: String,
    /// Display title.
    pub title: String,
    /// Display units.
    pub units: String,
    /// Lowest value.
    pub low: Option<f64>,
    /// Highest value.
    pub high: Option<f64>,
    /// Low danger threshold.
    pub low_danger: Option<f64>,
    /// Low warning threshold.
    pub low_warning: Option<f64>,
    /// High warning threshold.
    pub high_warning: Option<f64>,
    /// High danger threshold.
    pub high_danger: Option<f64>,
    /// Value digits.
    pub value_digits: Option<f64>,
    /// Label digits.
    pub label_digits: Option<f64>,
    /// Availability expression, as written.
    ///
    /// The template draws only while this holds; evaluation belongs to a
    /// future evaluator, like every other gate in the summaries.
    pub active: Option<String>,
    /// Source line, for a diagnostic or a tooltip.
    pub line: u32,
}

impl GaugeSummary {
    /// Shapes one parsed template for the UI.
    pub(crate) fn of(template: &Gauge) -> Self {
        Self {
            name: template.name.clone(),
            category: template.category.clone(),
            channel: template.channel.clone(),
            title: template.title.to_string(),
            units: template.units.to_string(),
            low: template.low.literal(),
            high: template.high.literal(),
            low_danger: template.low_danger.literal(),
            low_warning: template.low_warning.literal(),
            high_warning: template.high_warning.literal(),
            high_danger: template.high_danger.literal(),
            value_digits: template.value_digits.literal(),
            label_digits: template.label_digits.literal(),
            active: template.active_source.clone(),
            line: template.line,
        }
    }
}
