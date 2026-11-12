use std::collections::BTreeMap;

use crate::expr::Expr;
use crate::fields::CommandTemplate;

use super::{BitRange, DataType, Number, Text};

/// How a realtime channel gets its value.
#[expect(
    clippy::exhaustive_enums,
    reason = "a channel is either measured from the realtime block or computed"
)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub enum ChannelKind {
    /// Read from the realtime block at an offset.
    Measured {
        /// Wire type of the value.
        data_type: DataType,
        /// Type of the word a bit field is cut from (equals `data_type` for
        /// non-bit channels).
        storage: DataType,
        /// Byte offset in the realtime block.
        offset: u32,
        /// Bit range, for `bits` channels.
        bits: Option<BitRange>,
    },
    /// Computed from other channels and constants.
    Computed(Expr),
}

/// One realtime value.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct Channel {
    /// Channel name.
    pub name: String,
    /// Where the value comes from.
    pub kind: ChannelKind,
    /// Display units.
    pub units: Text,
    /// Scale, for measured channels.
    pub scale: Number,
    /// Translate, for measured channels.
    pub translate: Number,
    /// Whether the definition hides the channel from pickers.
    pub hidden: bool,
    /// Source line.
    pub line: u32,
    /// The entry as written.
    pub raw: String,
}

impl Channel {
    /// Whether the value is computed rather than read.
    pub fn is_computed(&self) -> bool {
        matches!(self.kind, ChannelKind::Computed(_))
    }
}

/// Scattered ("full optimized") realtime reads.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct ScatteredConfig {
    /// Command template used for a scattered read.
    pub get_command: Option<CommandTemplate>,
    /// Name of the U16 offset array constant.
    pub offset_array: Option<String>,
    /// Expression that says whether scattered reads are enabled.
    pub enabled: Option<String>,
}

/// Communication parameters, derived **after** settings resolution.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Default)]
pub struct CommsSettings {
    /// Maximum payload bytes per read/write chunk.
    pub blocking_factor: Option<u32>,
    /// Delay between written bytes, in milliseconds.
    pub inter_write_delay_ms: Option<u32>,
    /// Whether writes ignore `inter_write_delay`.
    pub write_blocks: bool,
    /// Maximum wait for a response block, in milliseconds.
    pub block_read_timeout_ms: Option<u32>,
    /// Wait after a page switch, in milliseconds.
    pub page_activation_delay_ms: Option<u32>,
    /// Wait after opening the port, in milliseconds.
    pub delay_after_port_open_ms: Option<u32>,
    /// Legacy reads have no delay.
    pub no_comm_read_delay: bool,
    /// Framing envelope, e.g. `msEnvelope_1.0`.
    pub envelope: Option<String>,
    /// Offsets and lengths are big-endian on the wire even when data is little.
    pub force_big_endian_protocol: bool,
    /// Command template that reads the realtime block.
    pub och_get_command: Option<CommandTemplate>,
    /// Size of the realtime block, in bytes.
    pub och_block_size: Option<u32>,
    /// Base added to realtime offsets.
    pub output_channel_start_offset: u32,
    /// Scattered-read configuration, when the definition provides one.
    pub scattered: Option<ScatteredConfig>,
    /// Default realtime sample rate.
    pub default_runtime_record_per_sec: Option<u32>,
    /// Channel-count hint for block sizing; 0 disables it.
    pub max_unused_runtime_range: u32,
    /// Command template that retrieves the configuration error text.
    pub retrieve_config_error: Option<CommandTemplate>,
    /// Command template that starts a burst read (`[BurstMode]`).
    pub burst_get_command: Option<CommandTemplate>,
    /// Every recognised communication key with its raw value, so nothing is
    /// lost before the protocol crate models it.
    ///
    /// Keys are ASCII-lowercased, whichever section declared them, so
    /// `tableWriteCommand` and `tablewritecommand` are the same entry.
    /// Synthetic keys built by the parser (`table.<id>.userpassword`,
    /// `curve.<name>.<key>`, `referenceTable.<name>.<key>`, `menu.<key>`) are
    /// namespaced instead: they keep the spelling of the names and keys they
    /// embed, so look those up verbatim.
    pub raw: BTreeMap<String, String>,
}
