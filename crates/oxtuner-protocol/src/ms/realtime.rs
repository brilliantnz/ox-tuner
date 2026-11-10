use oxtuner_ini::{CommandTemplate, Definition};

/// Where the realtime block lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RealtimeSpec {
    /// The command that fetches it.
    pub block_size: u16,
    /// The definition's `outputChannelStartOffset`.
    pub start_offset: u16,
}

/// The realtime command and its declared block.
#[derive(Debug, Clone)]
pub(super) struct Realtime {
    pub(super) command: CommandTemplate,
    pub(super) spec: RealtimeSpec,
}

pub(super) fn build_realtime(definition: &Definition) -> Option<Realtime> {
    definition
        .comms
        .och_get_command
        .clone()
        .zip(definition.comms.och_block_size)
        .and_then(|(command, block_size)| {
            let block_size = u16::try_from(block_size).ok()?;
            (block_size > 0).then(|| Realtime {
                command,
                spec: RealtimeSpec {
                    block_size,
                    start_offset: u16::try_from(definition.comms.output_channel_start_offset)
                        .unwrap_or(u16::MAX),
                },
            })
        })
}
