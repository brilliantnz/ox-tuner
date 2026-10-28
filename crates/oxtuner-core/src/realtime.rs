//! The realtime block: bytes to values, and the latest frame the UI reads.
//!
//! The definition drives everything: channel offsets, storage types, bit
//! fields, scales, translates and computed expressions. The decoder is built
//! once per connection, so a 10 Hz poll costs a walk over a small table rather
//! than a definition scan.
//!
//! The latest frame lives outside the event queue (ADR-0014): the poller writes
//! it, the UI reads it on its render tick, and a [`FrameWatch`] token says a
//! newer frame exists. The token is coalesced, so a stalled UI cannot back up
//! acquisition and a dropped wake shows the newer value, not an older one
//! (ADR-0021).

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crossfire::{AsyncRx, Rx, Tx, spsc};

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

/// One pending wake is enough: a token already waiting means the UI has not
/// looked yet, and the next frame will set the same signal again.
const WAKE_CAPACITY: usize = 1;

/// The core's end of the realtime wake signal.
///
/// Held by the core thread only, which is why it is not `Clone`: one producer,
/// one consumer (ADR-0019).
pub(crate) struct FrameNotify {
    wake: Tx<spsc::Array<()>>,
}

impl FrameNotify {
    /// Says a newer frame is readable. Never blocks and never fails: a full
    /// slot already means the same thing.
    pub(crate) fn wake(&self) {
        let _ = self.wake.try_send(());
    }
}

/// The UI's end of the realtime wake signal.
///
/// The token is not the frame: readers take the newest one from [`LatestFrame`]
/// and the chart history from [`crate::ChartHistory`], so several frames may
/// collapse into one wake without losing values.
pub struct FrameWatch {
    wake: AsyncRx<spsc::Array<()>>,
}

impl FrameWatch {
    /// Waits for the next frame to be announced.
    ///
    /// `false` means the core dropped its end, so no further wake will come and
    /// a loop should stop.
    pub async fn changed(&self) -> bool {
        self.wake.recv().await.is_ok()
    }

    /// Takes a pending wake without waiting.
    pub fn try_changed(&self) -> bool {
        self.wake.try_recv().is_ok()
    }

    /// Switches to the blocking end, for headless callers and tests.
    pub fn into_blocking(self) -> BlockingFrameWatch {
        BlockingFrameWatch {
            wake: self.wake.into_blocking(),
        }
    }
}

/// The blocking counterpart of [`FrameWatch`].
pub struct BlockingFrameWatch {
    wake: Rx<spsc::Array<()>>,
}

impl BlockingFrameWatch {
    /// Waits up to `timeout` for the next frame to be announced.
    pub fn wait(&self, timeout: std::time::Duration) -> bool {
        self.wake.recv_timeout(timeout).is_ok()
    }
}

/// Creates the wake pair: the core keeps the notifier, the UI takes the watch.
pub(crate) fn frame_wake() -> (FrameNotify, FrameWatch) {
    let (wake, watch) = spsc::bounded_blocking_async(WAKE_CAPACITY);
    (FrameNotify { wake }, FrameWatch { wake: watch })
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

    /// The visible channels, in definition order, with their units.
    ///
    /// These are the columns a session log writes and the channels a chart can
    /// offer. Hidden channels stay out, matching [`RealtimeFrame::values`].
    pub fn columns(&self) -> Vec<(String, String)> {
        self.sources
            .iter()
            .filter_map(|source| match source {
                Source::Measured {
                    name,
                    units,
                    hidden,
                    ..
                }
                | Source::Computed {
                    name,
                    units,
                    hidden,
                    ..
                } => (!hidden).then(|| (name.clone(), units.clone())),
            })
            .collect()
    }

    /// Whether the definition declares a measured channel with this name.
    ///
    /// The protocol reads a few status bits that no chart plots, so a caller
    /// can tell "this definition has no such channel" from "the block did not
    /// carry it".
    pub fn has_channel(&self, name: &str) -> bool {
        self.measured(name).is_some()
    }

    /// One measured channel's stored value, hidden or not.
    ///
    /// The value is taken after bit extraction and before scale and translate,
    /// which is what a status bit such as `burnPending` wants. A block of the
    /// wrong size returns `None`, like [`Self::decode`].
    pub fn channel_raw(&self, block: &[u8], name: &str) -> Option<f64> {
        if block.len() != self.block_size {
            return None;
        }
        let Some(Source::Measured {
            offset,
            data_type,
            storage,
            bits,
            ..
        }) = self.measured(name)
        else {
            return None;
        };
        let raw = read_raw(
            block,
            *offset,
            if bits.is_some() { *storage } else { *data_type },
            self.endianness,
        )?;
        match bits {
            Some(range) => extract_bits(raw, *range),
            None => Some(raw),
        }
    }

    /// The measured source for a channel name, case-insensitively.
    fn measured(&self, name: &str) -> Option<&Source> {
        self.sources.iter().find(|source| match source {
            Source::Measured { name: channel, .. } => channel.eq_ignore_ascii_case(name),
            Source::Computed { .. } => false,
        })
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
    use std::time::Duration;

    use oxtuner_ini::{SettingsContext, parse_str};

    use super::*;

    #[test]
    fn frame_wakes_coalesce_into_one_pending_token() {
        let (notify, watch) = frame_wake();
        let watch = watch.into_blocking();

        notify.wake();
        notify.wake();
        notify.wake();

        assert!(watch.wait(Duration::from_millis(50)), "a wake is pending");
        assert!(
            !watch.wait(Duration::from_millis(50)),
            "the extra wakes must not queue up behind the first"
        );
    }

    #[test]
    fn a_wake_never_waits_for_a_reader() {
        let (notify, _watch) = frame_wake();
        // The capacity is one; the fourth call still returns immediately
        // because a full slot already means "there is something to read".
        for _ in 0..100 {
            notify.wake();
        }
    }

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
burnPending = bits, U08, 2, [4:4]
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
    fn a_status_bit_reads_raw_without_decoding_the_frame() {
        let decoder = RealtimeDecoder::new(&definition());
        let mut block = vec![0u8; 8];

        block[2] = 0b0001_0000;
        assert_eq!(decoder.channel_raw(&block, "burnPending"), Some(1.0));
        assert_eq!(decoder.channel_raw(&block, "burnpending"), Some(1.0));
        assert!(decoder.has_channel("burnPending"));
        assert!(!decoder.has_channel("nothing"));

        block[2] = 0;
        assert_eq!(decoder.channel_raw(&block, "burnPending"), Some(0.0));

        // A short block says nothing, and so does a name that is not measured.
        assert_eq!(decoder.channel_raw(&[0u8; 4], "burnPending"), None);
        assert_eq!(decoder.channel_raw(&block, "halfRpm"), None);
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
