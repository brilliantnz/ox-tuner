use oxtuner_ini::CommandTemplate;
use oxtuner_transport::{Transport, TransportError};

use crate::error::ProtocolError;
use crate::frame::Frame;

use super::{COMMAND_REPLY_MAX, COMMAND_REPLY_QUIET, COMMAND_REPLY_TIMEOUT, LinkMode, MsProtocol};

/// What a `[ControllerCommands]` action sent and received.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RawCommandReport {
    /// Command strings sent.
    pub commands: usize,
    /// Bytes written to the wire, including framing.
    pub bytes: usize,
    /// Reply payload bytes, without framing.
    pub replies: usize,
}

impl MsProtocol {
    /// Sends one `[ControllerCommands]` action.
    ///
    /// Each command string is encoded in order and written as its own
    /// conversation. A legacy reply is drained until quiet; an envelope reply
    /// is read as one frame. Silence is not a failure.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError`] when a command cannot be encoded or the link
    /// fails. A timeout with no bytes is treated as a command that does not
    /// reply.
    pub fn run_commands(
        &mut self,
        io: &mut dyn Transport,
        commands: &[CommandTemplate],
    ) -> Result<RawCommandReport, ProtocolError> {
        let mut report = RawCommandReport::default();

        for command in commands {
            let request = self.encode(command, 0, 0, 0, &[])?;
            io.flush_input();

            match self.link {
                LinkMode::Legacy => {
                    // A raw action has no declared reply length, so whatever
                    // arrives is drained instead of parsed. Silence is normal:
                    // several actions (a reset, for one) never answer. Only a
                    // timeout with no bytes is silence; a partial reply is a
                    // desynchronised link, not success.
                    io.write_all(&request)?;

                    let mut reply = Vec::new();
                    match io.read_until(
                        &mut reply,
                        COMMAND_REPLY_MAX,
                        0,
                        COMMAND_REPLY_QUIET,
                        COMMAND_REPLY_TIMEOUT,
                    ) {
                        Ok(()) | Err(TransportError::Timeout { got: 0, .. }) => {}
                        Err(error) => return Err(error.into()),
                    }

                    report.bytes += request.len();
                    report.replies += reply.len();
                }
                LinkMode::Envelope => {
                    // An envelope reply has a length, so read it as a frame;
                    // only complete silence means the action did not answer.
                    // A partial frame is a desynchronised link, not success.
                    let framed = Frame::encode(&request)?;
                    io.write_all(&framed)?;

                    match self.framed_reply(io) {
                        Ok(payload) => report.replies += payload.len(),
                        Err(ProtocolError::Transport(TransportError::Timeout {
                            got: 0, ..
                        })) => {}
                        Err(error) => return Err(error),
                    }

                    report.bytes += framed.len();
                }
            }

            report.commands += 1;
        }

        // A raw action may answer after the drain window, and the next
        // conversation must not read those bytes as its own reply.
        io.flush_input();

        Ok(report)
    }
}
