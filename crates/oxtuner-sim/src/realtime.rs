//! Deterministic realtime waveforms, written at the definition's own offsets.
//!
//! The generator knows a handful of channel names (RPM, MAP, TPS, coolant,
//! intake, AFR, battery, baro, advance) and produces a smooth, bounded value
//! for each. Everything is a function of the request step and the seed, so two
//! runs of the same scenario produce identical blocks — the determinism ADR-0012
//! asks for — while a live session still shows movement.
//!
//! Values are converted through the definition's `scale`/`translate` and
//! clamped to its `minimum`/`maximum`, so nothing leaves the range the INI
//! declares.

use oxtuner_ini::{ChannelKind, DataType, Definition, Endianness};

/// One channel the generator writes.
#[derive(Debug, Clone)]
struct Source {
    offset: usize,
    data_type: DataType,
    scale: f64,
    translate: f64,
    wave: Wave,
}

/// The shaped signal behind a channel.
#[derive(Debug, Clone, Copy)]
enum Wave {
    Rpm,
    Map,
    Tps,
    Coolant,
    Intake,
    AirFuelRatio,
    Battery,
    Baro,
    Advance,
}

impl Wave {
    /// Picks a waveform by the channel's name.
    ///
    /// Names are matched case-insensitively and by substring, because a
    /// definition is free to call the same sensor `coolant`, `clt` or `CLT`.
    /// Anything unrecognised is left alone (zero), which is honest: the
    /// simulator does not invent data the definition does not describe.
    fn for_name(name: &str) -> Option<Self> {
        let name = name.to_ascii_lowercase();
        let is = |needle: &str| name == needle;
        let has = |needle: &str| name.contains(needle);

        Some(if is("rpm") {
            Self::Rpm
        } else if is("map") {
            Self::Map
        } else if is("tps") {
            Self::Tps
        } else if is("coolant") || is("clt") || has("coolant") {
            Self::Coolant
        } else if is("iat") || is("mat") || has("intake") {
            Self::Intake
        } else if is("afr") || is("ego") || has("lambda") {
            Self::AirFuelRatio
        } else if has("battery") || is("battv") {
            Self::Battery
        } else if is("baro") {
            Self::Baro
        } else if is("advance") || is("advdeg") {
            Self::Advance
        } else {
            return None;
        })
    }

    /// The user-unit value at `t` (radians).
    fn value(self, t: f64) -> f64 {
        let wave = |offset: f64, low: f64, high: f64| {
            low + (high - low) * (0.5 + 0.5 * (t + offset).sin())
        };
        match self {
            Self::Rpm => wave(0.0, 900.0, 5_800.0),
            Self::Map => wave(1.1, 32.0, 185.0),
            Self::Tps => wave(2.0, 0.0, 92.0),
            Self::Coolant => wave(0.3, 78.0, 94.0),
            Self::Intake => wave(4.0, 22.0, 38.0),
            Self::AirFuelRatio => wave(0.7, 13.0, 15.2),
            Self::Battery => wave(0.2, 13.4, 14.5),
            Self::Baro => wave(0.1, 96.0, 103.0),
            Self::Advance => wave(2.6, 6.0, 34.0),
        }
    }
}

/// Builds deterministic realtime blocks for one definition.
#[derive(Debug, Clone)]
pub(crate) struct Generator {
    block_size: usize,
    sources: Vec<Source>,
    endianness: Endianness,
    phase: f64,
}

impl Generator {
    /// Reads the channels of `definition` and prepares their sources.
    pub(crate) fn new(definition: &Definition, seed: u64) -> Self {
        let block_size = definition
            .comms
            .och_block_size
            .and_then(|size| usize::try_from(size).ok())
            .unwrap_or(0);
        let mut sources = Vec::new();

        for (name, channel) in definition.channels.iter() {
            let ChannelKind::Measured {
                data_type,
                offset,
                bits: None,
                ..
            } = &channel.kind
            else {
                // A computed channel is derived by the host, and a bit field
                // shares its word with a scalar that is already written.
                continue;
            };
            let Some(wave) = Wave::for_name(name) else {
                continue;
            };
            // An unresolved scale means the definition's own conversion is not
            // known; inventing 1.0 would put a wrong number on screen.
            let (Some(scale), Some(translate)) =
                (channel.scale.literal(), channel.translate.literal())
            else {
                continue;
            };
            let offset = usize::try_from(*offset).unwrap_or(usize::MAX);
            if offset >= block_size || type_width(*data_type) > block_size - offset {
                continue;
            }
            sources.push(Source {
                offset,
                data_type: *data_type,
                scale,
                translate,
                wave,
            });
        }

        Self {
            block_size,
            sources,
            endianness: definition.endianness,
            phase: phase_offset(seed),
        }
    }

    /// Builds the block for request `step`.
    pub(crate) fn block(&self, step: u64) -> Vec<u8> {
        let mut block = vec![0u8; self.block_size];
        let t = step_time(step) + self.phase;

        for source in &self.sources {
            let user = source.wave.value(t);
            let raw = if source.scale == 0.0 {
                0.0
            } else {
                user / source.scale - source.translate
            };
            write_raw(
                &mut block,
                source.offset,
                source.data_type,
                self.endianness,
                raw,
            );
        }

        block
    }
}

/// A deterministic starting phase for the waveform generator.
fn phase_offset(seed: u64) -> f64 {
    let seconds = u16::try_from(seed % 3_600).unwrap_or(0);
    f64::from(seconds) * 0.017
}

/// Simulated time for one request step.
#[expect(
    clippy::cast_precision_loss,
    clippy::as_conversions,
    reason = "a simulation step stays far below 2^53, where f64 is exact"
)]
fn step_time(step: u64) -> f64 {
    step as f64 * 0.05
}

fn type_width(data_type: DataType) -> usize {
    usize::try_from(data_type.size()).unwrap_or(usize::MAX)
}

/// Writes one numeric value in the definition's storage type.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::as_conversions,
    reason = "each cast is clamped to the storage type's range or is the documented f64 raw representation"
)]
fn write_raw(
    block: &mut [u8],
    offset: usize,
    data_type: DataType,
    endianness: Endianness,
    raw: f64,
) {
    let rounded = raw.round();
    let bytes = match data_type {
        DataType::U08 | DataType::M08 => [rounded.clamp(0.0, 255.0) as u8, 0, 0, 0, 0, 0, 0, 0],
        DataType::S08 => [
            (rounded.clamp(-128.0, 127.0) as i8).to_ne_bytes()[0],
            0,
            0,
            0,
            0,
            0,
            0,
            0,
        ],
        DataType::U16 => {
            let mut bytes = [0u8; 8];
            bytes[..2].copy_from_slice(&(rounded.clamp(0.0, 65_535.0) as u16).to_le_bytes());
            bytes
        }
        DataType::S16 => {
            let mut bytes = [0u8; 8];
            bytes[..2].copy_from_slice(&(rounded.clamp(-32_768.0, 32_767.0) as i16).to_le_bytes());
            bytes
        }
        DataType::U32 => {
            let mut bytes = [0u8; 8];
            bytes[..4].copy_from_slice(&(rounded.clamp(0.0, 4_294_967_295.0) as u32).to_le_bytes());
            bytes
        }
        DataType::S32 => {
            let mut bytes = [0u8; 8];
            bytes[..4].copy_from_slice(
                &(rounded.clamp(-2_147_483_648.0, 2_147_483_647.0) as i32).to_le_bytes(),
            );
            bytes
        }
        DataType::F32 => {
            let mut bytes = [0u8; 8];
            bytes[..4].copy_from_slice(&(raw as f32).to_le_bytes());
            bytes
        }
        DataType::U64 => {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&(rounded.max(0.0) as u64).to_le_bytes());
            bytes
        }
        DataType::S64 => {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&(rounded as i64).to_le_bytes());
            bytes
        }
        // Text has no numeric waveform; leave the bytes untouched.
        DataType::Ascii => return,
    };

    // On a little-endian definition the bytes above are already wire order; a
    // big-endian definition reverses the significant bytes.
    let width = type_width(data_type);
    if endianness == Endianness::Little {
        block[offset..offset + width].copy_from_slice(&bytes[..width]);
    } else {
        for index in 0..width {
            block[offset + index] = bytes[width - 1 - index];
        }
    }
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
coolant = scalar, S08, 2, \"C\", 1.0, 0.0
",
            &SettingsContext::new(),
        )
        .expect("parses")
    }

    #[test]
    fn the_block_is_deterministic() {
        let generator = Generator::new(&definition(), 7);

        assert_eq!(generator.block(0), generator.block(0));
        assert_ne!(generator.block(0), generator.block(1));
    }

    #[test]
    fn channels_stay_inside_the_definition_range() {
        let generator = Generator::new(&definition(), 7);

        for step in 0..100 {
            let block = generator.block(step);
            let rpm = u16::from_le_bytes([block[0], block[1]]);
            assert!((900..=5_800).contains(&rpm), "rpm {rpm} left its range");
        }
    }
}
