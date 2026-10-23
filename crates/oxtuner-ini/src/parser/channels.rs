//! `[OutputChannels]` and `[BurstMode]`.
//!
//! The section has three entry shapes on purpose: a
//! measurement (`scalar`/`bits` + offset), a derived value (`{ expression },
//! "units"`) and the legacy row without a class token. All three appear in real
//! Speeduino and rusEFI files.

use crate::fields::{Command, split_fields};
use crate::model::{
    BitRange, Channel, ChannelKind, DataType, Definition, Number, ScatteredConfig, Text,
};
use crate::preprocess::Expander;

use super::SectionParser;

/// `[OutputChannels]`.
pub(crate) fn parse_channels(
    definition: &mut Definition,
    section: &mut SectionParser,
    lines: &[(u32, &str)],
    expander: &Expander<'_>,
) {
    for (line, text) in lines {
        let Some((key, value)) = super::entry_line(section, *line, text) else {
            continue;
        };

        match key.to_ascii_lowercase().as_str() {
            "ochgetcommand" => {
                definition.protocol.och_get_command = Some(Command::parse(&value));
                section.understand(*line);
                continue;
            }
            "ochblocksize" => {
                definition.protocol.och_block_size = crate::fields::parse_u32(&value);
                section.understand(*line);
                continue;
            }
            "maxunusedruntimerange" => {
                // An unreadable value keeps the dialect default instead of guessing.
                if let Some(range) = crate::fields::parse_u32(&value) {
                    definition.protocol.max_unused_runtime_range = range;
                } else {
                    section.warn(
                        "bad-number",
                        format!(
                            "`maxUnusedRuntimeRange` is not a number: {value}; keeping the \
                             dialect default"
                        ),
                        *line,
                    );
                }
                section.understand(*line);
                continue;
            }
            "scatteredochgetcommand" => {
                let scattered = definition
                    .protocol
                    .scattered
                    .get_or_insert_with(ScatteredConfig::default);
                scattered.get_command = Some(Command::parse(&value));
                section.understand(*line);
                continue;
            }
            "scatteredoffsetarray" => {
                let scattered = definition
                    .protocol
                    .scattered
                    .get_or_insert_with(ScatteredConfig::default);
                scattered.offset_array = Some(crate::fields::unquote(&value).to_owned());
                section.understand(*line);
                continue;
            }
            "scatteredgetenabled" => {
                let scattered = definition
                    .protocol
                    .scattered
                    .get_or_insert_with(ScatteredConfig::default);
                scattered.enabled = Some(value.clone());
                section.understand(*line);
                continue;
            }
            _ => {}
        }

        let fields = expander.expand(&split_fields(&value));
        match parse_channel(&key, &value, &fields, *line) {
            Ok(channel) => {
                let name = channel.name.clone();
                if definition.channels.insert(&name, channel).is_some() {
                    section.error(
                        "duplicate-name",
                        format!("channel `{name}` is defined more than once; the later wins"),
                        *line,
                    );
                }
                section.understand(*line);
            }
            Err(message) => section.warn("bad-channel", format!("`{key}`: {message}"), *line),
        }
    }
}

fn parse_channel(name: &str, raw: &str, fields: &[String], line: u32) -> Result<Channel, String> {
    let first = fields.first().ok_or("no value")?.trim().to_owned();

    // Derived channel: `name = { ego1 / 10.0 }, "AFR"`.
    if crate::fields::is_expression_field(&first) {
        let source = crate::fields::expression_body(&first).unwrap_or("");
        let expression = crate::expr::Expr::parse(source)
            .map_err(|error| format!("expression {source:?}: {error}"))?;
        let units = fields
            .get(1)
            .map(|field| Text::parse(field))
            .unwrap_or(Text::Literal(String::new()));
        return Ok(Channel {
            name: name.to_owned(),
            kind: ChannelKind::Computed(expression),
            units,
            scale: Number::Literal(1.0),
            translate: Number::Literal(0.0),
            hidden: false,
            line,
            raw: raw.to_owned(),
        });
    }

    let lowered = first.to_ascii_lowercase();
    let mut index = match lowered.as_str() {
        "scalar" | "bits" | "array" => 1usize,
        // Legacy row: `rpm = U16, 0, "RPM", 1.0, 0.0`.
        _ => {
            if DataType::parse(&first).is_some() {
                0usize
            } else {
                return Err(format!("{:?} is not a channel class", first));
            }
        }
    };

    let data_type = fields
        .get(index)
        .and_then(|field| DataType::parse(field))
        .ok_or_else(|| {
            format!(
                "data type {:?}",
                fields.get(index).map(String::as_str).unwrap_or("")
            )
        })?;
    index += 1;

    let offset_field = fields.get(index).ok_or("no offset")?;
    let offset = match offset_field.trim().to_ascii_lowercase().as_str() {
        "nextoffset" | "lastoffset" => {
            // Realtime offsets are absolute in every real INI; a relative
            // keyword here has no previous element to be relative to.
            index += 1;
            0
        }
        _ => {
            let offset = crate::fields::parse_u32(offset_field)
                .ok_or_else(|| format!("offset {offset_field:?}"))?;
            index += 1;
            offset
        }
    };

    let mut bits = None;
    if let Some(field) = fields.get(index)
        && field.trim().starts_with('[')
    {
        bits = parse_bit_range(field);
        index += 1;
    }

    let mut units = Text::Literal(String::new());
    if let Some(field) = fields.get(index)
        && (field.starts_with('"') || crate::fields::is_expression_field(field))
    {
        units = Text::parse(field);
        index += 1;
    }

    let scale = fields
        .get(index)
        .map(|field| Number::parse(field))
        .unwrap_or(Number::Literal(1.0));
    let translate = fields
        .get(index + 1)
        .map(|field| Number::parse(field))
        .unwrap_or(Number::Literal(0.0));

    let kind = if lowered == "bits" {
        ChannelKind::Measured {
            data_type: DataType::U08,
            storage: data_type,
            offset,
            bits,
        }
    } else {
        ChannelKind::Measured {
            data_type,
            storage: data_type,
            offset,
            bits: None,
        }
    };

    Ok(Channel {
        name: name.to_owned(),
        kind,
        units,
        scale,
        translate,
        hidden: false,
        line,
        raw: raw.to_owned(),
    })
}

fn parse_bit_range(field: &str) -> Option<BitRange> {
    let inner = field.trim().strip_prefix('[')?.strip_suffix(']')?;
    let (low, high) = inner.split_once(':')?;
    let low: u8 = low.trim().parse().ok()?;
    let high: u8 = high.trim().parse().ok()?;
    (low <= high).then_some(BitRange { low, high })
}

/// `[BurstMode]`: the fast whole-block read command.
pub(crate) fn parse_burst_mode(
    definition: &mut Definition,
    section: &mut SectionParser,
    lines: &[(u32, &str)],
) {
    for (line, text) in lines {
        let Some((key, value)) = super::entry_line(section, *line, text) else {
            continue;
        };
        match key.to_ascii_lowercase().as_str() {
            "getcommand" => {
                definition.protocol.burst_get_command = Some(Command::parse(&value));
                section.understand(*line);
            }
            _ => section.unrecognized_key(&key, *line),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scalar_channel_reads_its_type_offset_and_units() {
        let channel = parse_channel(
            "map",
            "scalar, U16, 4, \"kpa\", 1.000, 0.000",
            &split_fields("scalar, U16, 4, \"kpa\", 1.000, 0.000"),
            1,
        )
        .expect("parses");

        assert_eq!(channel.units.literal(), Some("kpa"));
        match channel.kind {
            ChannelKind::Measured {
                data_type, offset, ..
            } => {
                assert_eq!(data_type, DataType::U16);
                assert_eq!(offset, 4);
            }
            ChannelKind::Computed(_) => panic!("a scalar is not computed"),
        }
    }

    #[test]
    fn a_bits_channel_keeps_the_width_it_is_measured_against() {
        // Reading only the start bit of `nSquirts` reports 0, and the INI's
        // `pulseLimit = { cycleTime / nSquirts }` then divides by zero.
        let channel = parse_channel(
            "nSquirts",
            "bits, U08, 84, [5:7]",
            &split_fields("bits, U08, 84, [5:7]"),
            1,
        )
        .expect("parses");

        match channel.kind {
            ChannelKind::Measured { storage, bits, .. } => {
                assert_eq!(storage, DataType::U08);
                let bits = bits.expect("bit range");
                assert_eq!(bits.width(), 3);
            }
            ChannelKind::Computed(_) => panic!("a bits channel is measured"),
        }
    }

    #[test]
    fn a_computed_channel_keeps_its_expression() {
        let channel = parse_channel(
            "afr",
            "{ ego1 / 10.0 }, \"AFR\"",
            &split_fields("{ ego1 / 10.0 }, \"AFR\""),
            1,
        )
        .expect("parses");

        assert!(channel.is_computed());
        assert_eq!(channel.units.literal(), Some("AFR"));
    }

    #[test]
    fn the_legacy_row_without_a_class_token_still_parses() {
        let channel = parse_channel(
            "rpm",
            "U16, 0, \"RPM\", 1.0, 0.0",
            &split_fields("U16, 0, \"RPM\", 1.0, 0.0"),
            1,
        )
        .expect("parses");

        assert_eq!(channel.units.literal(), Some("RPM"));
    }
}
