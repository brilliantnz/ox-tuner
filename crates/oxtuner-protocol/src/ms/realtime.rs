use oxtuner_ini::{CommandTemplate, Definition};

use crate::error::ProtocolError;

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

pub(super) fn build_realtime(definition: &Definition) -> Result<Option<Realtime>, ProtocolError> {
    let Some(command) = definition.comms.och_get_command.clone() else {
        return Ok(None);
    };
    let Some(block_size) = definition
        .comms
        .och_block_size
        .and_then(|size| u16::try_from(size).ok())
        .filter(|size| *size > 0)
    else {
        return Ok(None);
    };

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
