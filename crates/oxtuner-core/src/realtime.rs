//! The realtime block: bytes to values, and the latest frame the UI reads.
//!
//! The definition drives everything: channel offsets, storage types, bit
//! fields, scales, translates and computed expressions. The decoder is built
//! once per connection, so a 10 Hz poll costs a walk over a small table rather
//! than a definition scan.
//!
//! The latest frame lives outside the event queue (ADR-0014): the poller writes
//! it, the UI reads it on its render tick, and an [`Event::RealtimeFrame`]
//! carries only the sequence number that wakes the view. A slow frame therefore
//! never backs up rendering, and a dropped repaint shows the newer value, not
//! an older one.
//!
//! [`Event::RealtimeFrame`]: crate::Event::RealtimeFrame

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use oxtuner_ini::{BitRange, ChannelKind, DataType, Definition, Endianness, Number};

/// One decoded channel.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveValue {
    /// Channel name as written in the definition.
    pub name: String,
    /// The value in the channel's user units.
    pub value: f64,
    /// The value formatted for display (respects the channel's units).
    pub text: String,
    /// Display units.
    pub units: String,
}

/// One realtime block, decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct RealtimeFrame {
    /// Increases with every frame received in this session.
    pub sequence: u64,
    /// Channels in definition order, hidden ones excluded.
    pub values: Vec<LiveValue>,
}

impl RealtimeFrame {
    /// One channel by name, case-insensitively.
    pub fn value(&self, name: &str) -> Option<&LiveValue> {
        self.values
            .iter()
            .find(|value| value.name.eq_ignore_ascii_case(name))
    }
}

/// The freshest frame, shared between the core and the UI.
///
/// Cloneable and cheap to read: the core thread replaces the `Arc`, the UI
/// clones it once per render. Nothing blocks for longer than a pointer swap.
#[derive(Debug, Clone, Default)]
pub struct LatestFrame {
    inner: Arc<RwLock<Option<Arc<RealtimeFrame>>>>,
}

impl LatestFrame {
    /// The newest frame, or `None` before the first one arrives.
    pub fn get(&self) -> Option<Arc<RealtimeFrame>> {
        self.inner
            .read()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    /// Replaces the frame. Called by the core's poller only.
    pub(crate) fn set(&self, frame: RealtimeFrame) {
        let mut slot = self
            .inner
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        *slot = Some(Arc::new(frame));
    }

    /// Forgets the frame (a disconnect, or a definition change).
    pub(crate) fn clear(&self) {
        let mut slot = self
            .inner
            .write()
            .unwrap_or_else(|poison| poison.into_inner());
        *slot = None;
    }
}

/// One decoded channel definition.
#[derive(Debug, Clone)]
enum Source {
    Measured {
        name: String,
        offset: usize,
        data_type: DataType,
        storage: DataType,
        bits: Option<BitRange>,
        scale: f64,
        translate: f64,
        units: String,
        hidden: bool,
    },
    Computed {
        name: String,
        expression: oxtuner_ini::expr::Expr,
        scale: f64,
        translate: f64,
        units: String,
        hidden: bool,
    },
}

/// Reads realtime blocks for one definition.
#[derive(Debug, Clone)]
pub struct RealtimeDecoder {
    sources: Vec<Source>,
    endianness: Endianness,
    block_size: usize,
}

impl RealtimeDecoder {
    /// Prepares the decoder from the definition's `[OutputChannels]`.
    pub fn new(definition: &Definition) -> Self {
        let mut sources = Vec::with_capacity(definition.channels.len());

        for (name, channel) in definition.channels.iter() {
            let Some(scale) = literal(&channel.scale) else {
                // An unresolved scale cannot produce a truthful user value.
                continue;
            };
            let Some(translate) = literal(&channel.translate) else {
                continue;
            };
            let units = channel.units.literal().unwrap_or_default().to_owned();
            match &channel.kind {
                ChannelKind::Measured {
                    data_type,
                    storage,
                    offset,
                    bits,
                } => {
                    sources.push(Source::Measured {
                        name: name.to_owned(),
                        offset: usize::try_from(*offset).unwrap_or(usize::MAX),
                        data_type: *data_type,
                        storage: *storage,
                        bits: *bits,
                        scale,
                        translate,
                        units,
                        hidden: channel.hidden,
                    });
                }
                ChannelKind::Computed(expression) => {
                    sources.push(Source::Computed {
                        name: name.to_owned(),
                        expression: expression.clone(),
                        scale,
                        translate,
                        units,
                        hidden: channel.hidden,
                    });
                }
            }
        }

        Self {
            sources,
            endianness: definition.endianness,
            block_size: definition
                .protocol
                .och_block_size
                .and_then(|size| usize::try_from(size).ok())
                .unwrap_or(0),
        }
    }

    /// The block size the definition declares.
    pub fn block_size(&self) -> usize {
        self.block_size
    }

    /// Decodes one block.
    ///
    /// A block whose length is not the declared one produces no values: a
    /// short frame would otherwise show stale or shifted data as if it were
    /// current.
    pub fn decode(&self, block: &[u8], sequence: u64) -> RealtimeFrame {
        if block.len() != self.block_size {
            tracing::warn!(
                expected = self.block_size,
                got = block.len(),
                "the realtime block is not the declared size"
            );
            return RealtimeFrame {
                sequence,
                values: Vec::new(),
            };
        }

        let mut known: HashMap<String, f64> = HashMap::with_capacity(self.sources.len());
        let mut values = Vec::with_capacity(self.sources.len());

        for source in &self.sources {
            let Source::Measured {
                name,
                offset,
                data_type,
                storage,
                bits,
                scale,
                translate,
                units,
                hidden,
            } = source
            else {
                continue;
            };
            let Some(raw) = read_raw(
                block,
                *offset,
                if bits.is_some() { *storage } else { *data_type },
                self.endianness,
            ) else {
                continue;
            };
            let raw = match bits {
                Some(range) => extract_bits(raw, *range),
                None => Some(raw),
            };
            let Some(raw) = raw else {
                continue;
            };
            let user = (raw + translate) * scale;
            known.insert(name.to_ascii_lowercase(), user);
            if !hidden {
                values.push(LiveValue {
                    name: name.clone(),
                    value: user,
                    text: format_value(user),
                    units: units.clone(),
                });
            }
        }

        // Computed channels come second: most expressions reference measured
        // values, and a chain of computed channels resolves in declaration
        // order.
        for source in &self.sources {
            let Source::Computed {
                name,
                expression,
                scale,
                translate,
                units,
                hidden,
            } = source
            else {
                continue;
            };
            let lookup = |name: &str| known.get(&name.to_ascii_lowercase()).copied();
            let Some(raw) = expression.evaluate(&lookup) else {
                continue;
            };
            let user = (raw + translate) * scale;
            if !user.is_finite() {
                continue;
            }
            known.insert(name.to_ascii_lowercase(), user);
            if !hidden {
                values.push(LiveValue {
                    name: name.clone(),
                    value: user,
                    text: format_value(user),
                    units: units.clone(),
                });
            }
        }

        RealtimeFrame { sequence, values }
    }
}

fn literal(number: &Number) -> Option<f64> {
    number.literal().filter(|value| value.is_finite())
}

fn format_value(value: f64) -> String {
    if !value.is_finite() {
        return "—".to_owned();
    }
    if value.fract().abs() < 1e-9 {
        format!("{value:.0}")
    } else if value.abs() >= 100.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.2}")
    }
}

/// Reads one numeric value from the block.
fn read_raw(
    bytes: &[u8],
    offset: usize,
    data_type: DataType,
    endianness: Endianness,
) -> Option<f64> {
    let width = data_type.size() as usize;
    let slice = bytes.get(offset..offset.checked_add(width)?)?;
    let mut ordered = [0u8; 8];
    match endianness {
        Endianness::Little => ordered[..width].copy_from_slice(slice),
        Endianness::Big => {
            for index in 0..width {
                ordered[index] = slice[width - 1 - index];
            }
        }
    }
    Some(match data_type {
        DataType::U08 | DataType::M08 => f64::from(ordered[0]),
        DataType::S08 => f64::from(ordered[0] as i8),
        DataType::U16 => f64::from(u16::from_le_bytes([ordered[0], ordered[1]])),
        DataType::S16 => f64::from(i16::from_le_bytes([ordered[0], ordered[1]])),
        DataType::U32 => f64::from(u32::from_le_bytes([
            ordered[0], ordered[1], ordered[2], ordered[3],
        ])),
        DataType::S32 => f64::from(i32::from_le_bytes([
            ordered[0], ordered[1], ordered[2], ordered[3],
        ])),
        DataType::F32 => f64::from(f32::from_le_bytes([
            ordered[0], ordered[1], ordered[2], ordered[3],
        ])),
        DataType::U64 => u64::from_le_bytes(ordered) as f64,
        DataType::S64 => i64::from_le_bytes(ordered) as f64,
        DataType::Ascii => return None,
    })
}

fn extract_bits(raw: f64, range: BitRange) -> Option<f64> {
    if !raw.is_finite() || raw < 0.0 {
        return None;
    }
    let word = raw as u64;
    let width = range
        .high
        .saturating_sub(range.low)
        .saturating_add(1)
        .min(64);
    if width == 0 {
        return None;
    }
    let mask = if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    };
    Some(((word >> range.low) & mask) as f64)
}

#[cfg(test)]
mod tests {
    use oxtuner_ini::{SettingsContext, parse_str};

    use super::*;

    fn definition() -> Definition {
        parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
endianness = little
nPages = 1
pageSize = 8
page = 1
reqFuel = scalar, U08, 0, \"ms\", 0.1, 0, 0, 25.5, 1

[OutputChannels]
ochGetCommand = \"r%2o%2c\"
ochBlockSize = 8
rpm = scalar, U16, 0, \"rpm\", 1.0, 0.0
status = scalar, U08, 2, \"bits\", 1.0, 0.0
running = bits, U08, 2, [0:0]
tps = scalar, U08, 3, \"%\", 0.5, 0.0
halfRpm = { rpm / 2 }, \"rpm\"
",
            &SettingsContext::new(),
        )
        .expect("parses")
    }

    #[test]
    fn measured_channels_decode_with_their_scale() {
        let decoder = RealtimeDecoder::new(&definition());
        let mut block = vec![0u8; 8];
        block[..2].copy_from_slice(&3500u16.to_le_bytes());
        block[2] = 1;
        block[3] = 40;

        let frame = decoder.decode(&block, 1);

        assert_eq!(frame.value("rpm").unwrap().value, 3500.0);
        assert_eq!(frame.value("tps").unwrap().value, 20.0);
        assert_eq!(frame.value("running").unwrap().value, 1.0);
    }

    #[test]
    fn a_computed_channel_resolves_against_measured_ones() {
        let decoder = RealtimeDecoder::new(&definition());
        let mut block = vec![0u8; 8];
        block[..2].copy_from_slice(&3500u16.to_le_bytes());

        let frame = decoder.decode(&block, 1);

        assert_eq!(frame.value("halfRpm").unwrap().value, 1750.0);
    }

    #[test]
    fn a_short_block_is_not_decoded() {
        let decoder = RealtimeDecoder::new(&definition());

        let frame = decoder.decode(&[0u8; 4], 1);

        assert!(frame.values.is_empty());
    }

    #[test]
    fn the_latest_frame_can_be_read_and_cleared() {
        let latest = LatestFrame::default();
        assert!(latest.get().is_none());

        latest.set(RealtimeFrame {
            sequence: 3,
            values: Vec::new(),
        });
        assert_eq!(latest.get().unwrap().sequence, 3);

        latest.clear();
        assert!(latest.get().is_none());
    }
}
