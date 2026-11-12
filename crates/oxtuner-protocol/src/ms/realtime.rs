use oxtuner_ini::{CommandTemplate, Definition};

use crate::error::ProtocolError;

/// Where the realtime block lives.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RealtimeSpec {
    /// The definition's `ochBlockSize`: payload bytes in one reply.
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

pub(super) fn build_realtime(definition: &Definition) -> Result<Option<Realtime>, ProtocolError> {
    let Some(command) = definition.comms.och_get_command.clone() else {
        return Ok(None);
    };
    let Some(size) = definition.comms.och_block_size else {
        return Ok(None);
    };
    let block_size = u16::try_from(size)
        .ok()
        .filter(|size| *size > 0)
        .ok_or_else(|| {
            ProtocolError::Unsupported(format!(
                "the realtime block is {size} byte(s), which is not a usable 16-bit size"
            ))
        })?;

    let start_offset =
        u16::try_from(definition.comms.output_channel_start_offset).map_err(|_| {
            ProtocolError::Unsupported(format!(
                "the realtime block starts at offset {}, which does not fit the 16-bit wire field",
                definition.comms.output_channel_start_offset
            ))
        })?;

    Ok(Some(Realtime {
        command,
        spec: RealtimeSpec {
            block_size,
            start_offset,
        },
    }))
}
