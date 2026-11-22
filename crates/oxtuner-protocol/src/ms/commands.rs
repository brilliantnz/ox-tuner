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
            let (bytes, reply) = self.command_reply(io, command)?;
            report.bytes += bytes;
            report.replies += reply.len();
            report.commands += 1;
        }

        // A raw action may answer after the drain window, and the next
        // conversation must not read those bytes as its own reply.
        io.flush_input();

        Ok(report)
    }

    /// Sends one command and returns its reply text.
    ///
    /// This is the `versionInfo` interrogation, not a `[ControllerCommands]`
    /// action: the reply is firmware metadata the caller keeps. It uses the
    /// same bounded drain as an action, trims the first line and caps the text
    /// so a malformed or noisy controller cannot flood a header.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError`] when the command cannot be encoded or the link
    /// fails. Silence is an empty string, not an error.
    pub fn read_command_text(
        &mut self,
        io: &mut dyn Transport,
        command: &CommandTemplate,
    ) -> Result<String, ProtocolError> {
        let (_, reply) = self.command_reply(io, command)?;
        io.flush_input();

        Ok(reply_text(&reply))
    }

    /// One command conversation: bytes written and reply payload.
    fn command_reply(
        &mut self,
        io: &mut dyn Transport,
        command: &CommandTemplate,
    ) -> Result<(usize, Vec<u8>), ProtocolError> {
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

                Ok((request.len(), reply))
            }
            LinkMode::Envelope => {
                // An envelope reply has a length, so read it as a frame;
                // only complete silence means the action did not answer.
                // A partial frame is a desynchronised link, not success.
                let framed = Frame::encode(&request)?;
                io.write_all(&framed)?;

                match self.framed_reply(io) {
                    Ok(payload) => Ok((framed.len(), payload)),
                    Err(ProtocolError::Transport(TransportError::Timeout { got: 0, .. })) => {
                        Ok((framed.len(), Vec::new()))
                    }
                    Err(error) => Err(error),
                }
            }
        }
    }
}

/// The first line of a command reply, trimmed and capped.
fn reply_text(reply: &[u8]) -> String {
    let text = String::from_utf8_lossy(reply);
    let line = text
        // A firmware reply often starts with a serial return code byte
        // (`SERIAL_RC_OK`) before the text; it is not part of the version.
        .trim_start_matches(char::is_control)
        .split(['\0', '\r', '\n'])
        .next()
        .unwrap_or_default()
        .trim();

    line.chars().take(MAX_VERSION_TEXT).collect()
}

/// Longest firmware version text kept from a controller reply.
const MAX_VERSION_TEXT: usize = 128;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_reply_keeps_the_first_trimmed_line() {
        assert_eq!(
            reply_text(b"Speeduino 2025.01.7\0\r\n"),
            "Speeduino 2025.01.7"
        );
        assert_eq!(
            reply_text(b"\0Speeduino 2025.01.7"),
            "Speeduino 2025.01.7",
            "a leading serial return code is not part of the version"
        );
        assert_eq!(reply_text(b"  \r\nnext"), "");
        assert_eq!(reply_text(b""), "");
    }

    #[test]
    fn a_version_reply_is_capped() {
        let long = "x".repeat(MAX_VERSION_TEXT + 40);

        assert_eq!(reply_text(long.as_bytes()).len(), MAX_VERSION_TEXT);
    }
}
