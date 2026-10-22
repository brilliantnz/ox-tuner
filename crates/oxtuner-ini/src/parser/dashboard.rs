//! `[GaugeConfigurations]` and `[FrontPage]`.
//!
//! These sections are pure presentation data: limits, colours and digits. The
//! model keeps them so the UI can *decide* how to draw a gauge; the reference
//! documents no rendering rules and this crate does not invent any.

use crate::fields::split_fields;
use crate::model::{Definition, Gauge, Indicator, Number, Text};

use super::SectionParser;

/// `[GaugeConfigurations]`.
pub(crate) fn parse_gauges(
    definition: &mut Definition,
    section: &mut SectionParser,
    lines: &[(u32, &str)],
) {
    let mut category: Option<String> = None;

    for (line, text) in lines {
        let Some((key, value)) = super::entry_line(section, *line, text) else {
            continue;
        };
        let fields = split_fields(&value);

        if key.eq_ignore_ascii_case("gaugeCategory") {
            category = fields
                .first()
                .map(|field| crate::fields::unquote(field).to_owned());
            section.understand(*line);
            continue;
        }

        // channel, "Title", "Units", lo, hi, loD, loW, hiW, hiD, vd, ld[, active]
        // The trailing label-digits field is optional in the wild; rusEFI
        // templates omit it.
        if fields.len() < 10 {
            section.warn(
                "bad-gauge",
                format!(
                    "`{key}` needs at least 10 fields (channel, title, units, six limits and value digits), got {}",
                    fields.len()
                ),
                *line,
            );
            continue;
        }

        let gauge = Gauge {
            category: category.clone(),
            name: key.clone(),
            channel: fields[0].clone(),
            title: Text::parse(&fields[1]),
            units: Text::parse(&fields[2]),
            low: Number::parse(&fields[3]),
            high: Number::parse(&fields[4]),
            low_danger: Number::parse(&fields[5]),
            low_warning: Number::parse(&fields[6]),
            high_warning: Number::parse(&fields[7]),
            high_danger: Number::parse(&fields[8]),
            value_digits: Number::parse(&fields[9]),
            label_digits: fields
                .get(10)
                .map(|field| Number::parse(field))
                .unwrap_or_else(|| Number::parse(&fields[9])),
            active: fields
                .get(11)
                .and_then(|field| crate::fields::expression_body(field))
                .and_then(|source| crate::expr::Expr::parse(source).ok()),
            line: *line,
        };

        if !definition.channels.contains(&gauge.channel) {
            section.note(
                "gauge-without-channel",
                format!(
                    "gauge `{}` shows `{}`, which is not an output channel",
                    gauge.name, gauge.channel
                ),
                *line,
            );
        }
        definition.gauges.push(gauge);
        section.understand(*line);
    }
}

/// `[FrontPage]`.
pub(crate) fn parse_front_page(
    definition: &mut Definition,
    section: &mut SectionParser,
    lines: &[(u32, &str)],
) {
    for (line, text) in lines {
        let Some((key, value)) = super::entry_line(section, *line, text) else {
            continue;
        };
        let lowered = key.to_ascii_lowercase();

        if let Some(rest) = lowered.strip_prefix("gauge")
            && let Ok(slot) = rest.parse::<u8>()
        {
            let template = crate::fields::unquote(&value).to_owned();
            definition
                .front_page
                .gauges
                .retain(|(existing, _)| *existing != slot);
            definition.front_page.gauges.push((slot, template));
            definition.front_page.gauges.sort_by_key(|(slot, _)| *slot);
            section.understand(*line);
            continue;
        }

        if lowered == "indicator" || lowered == "indicatortemplate" {
            let fields = split_fields(&value);
            if fields.len() < 3 {
                section.warn(
                    "bad-indicator",
                    format!("`{key}` needs a channel, an off label and an on label"),
                    *line,
                );
                continue;
            }
            definition.front_page.indicators.push(Indicator {
                channel: crate::fields::unquote(&fields[0]).to_owned(),
                off_label: crate::fields::unquote(&fields[1]).to_owned(),
                on_label: crate::fields::unquote(&fields[2]).to_owned(),
                colours: fields[3..].to_vec(),
                template_only: lowered == "indicatortemplate",
                line: *line,
            });
            section.understand(*line);
            continue;
        }

        section.unrecognized_key(&key, *line);
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
    fn a_gauge_keeps_its_limits_and_category() {
        let definition = parse(
            "[MegaTune]\nsignature = \"x\"\n[GaugeConfigurations]\ngaugeCategory = \"Fuel\"\nrpmGauge = rpm, \"Engine Speed\", \"RPM\", 0, 7000, 0, 500, 6000, 7000, 0, 0\n",
        );

        let gauge = definition.gauges.first().expect("one gauge");
        assert_eq!(gauge.category.as_deref(), Some("Fuel"));
        assert_eq!(gauge.channel, "rpm");
        assert_eq!(gauge.high.literal(), Some(7000.0));
        assert_eq!(gauge.title.literal(), Some("Engine Speed"));
    }

    #[test]
    fn front_page_gauges_keep_their_slot_order() {
        let definition =
            parse("[MegaTune]\nsignature = \"x\"\n[FrontPage]\ngauge2 = b\ngauge1 = a\n");

        assert_eq!(
            definition.front_page.slots(),
            [(1, "a".to_owned()), (2, "b".to_owned())]
        );
    }

    #[test]
    fn a_template_only_indicator_is_marked_as_such() {
        let definition = parse(
            "[MegaTune]\nsignature = \"x\"\n[FrontPage]\nindicatorTemplate = { x }, \"Off\", \"On\", white, black, red, black\n",
        );

        assert!(definition.front_page.indicators[0].template_only);
        assert_eq!(definition.front_page.indicators[0].off_label, "Off");
    }
}
