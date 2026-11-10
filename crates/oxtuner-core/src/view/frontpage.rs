use oxtuner_ini::Gauge;

use crate::contract::frontpage::GaugeTemplate;

impl GaugeTemplate {
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
