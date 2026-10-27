//! Protocol failures, one layer above the transport.

use std::fmt;

use oxtuner_transport::TransportError;

/// Why a conversation produced no usable answer.
///
/// The variants name what the caller can decide: reconnect, reject the reply,
/// or refuse the request. They never contain a suggested retry (S6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// The link failed.
    Transport(TransportError),
    /// The definition does not describe this operation (no command, no page).
    Unsupported(String),
    /// The reply arrived but does not fit the expected shape.
    UnexpectedReply(String),
    /// A framed reply failed its CRC or its length did not add up.
    BadFrame(String),
    /// The ECU answered with a non-zero return code.
    ReturnCode {
        /// The code the ECU sent.
        code: u8,
        /// The operation that was refused.
        operation: &'static str,
    },
    /// The definition's command template could not be encoded.
    Encoding(String),
}

impl ProtocolError {
    /// Whether this failure means the link itself is unusable, so the session
    /// should move to its reconnect state (ADR-0008).
    pub fn is_link_loss(&self) -> bool {
        matches!(
            self,
            Self::Transport(
                TransportError::Closed
                    | TransportError::Read(_)
                    | TransportError::Write(_)
                    | TransportError::NotOpen
            )
        )
    }

    /// Whether the ECU stayed silent, which is a timeout rather than a broken
    /// link.
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Transport(TransportError::Timeout { .. }))
    }
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "{error}"),
            Self::Unsupported(what) => write!(formatter, "{what}"),
            Self::UnexpectedReply(message) => write!(formatter, "unexpected reply: {message}"),
            Self::BadFrame(message) => write!(formatter, "bad frame: {message}"),
            Self::ReturnCode { code, operation } => {
                write!(formatter, "the ECU refused {operation} (code 0x{code:02x})")
            }
            Self::Encoding(message) => write!(formatter, "could not encode the command: {message}"),
        }
    }
}

impl std::error::Error for ProtocolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            _ => None,
        }
    }
}

impl From<TransportError> for ProtocolError {
    fn from(error: TransportError) -> Self {
        Self::Transport(error)
    }
}
