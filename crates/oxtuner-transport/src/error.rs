//! Transport failures, kept free of protocol or UI meaning.

use std::fmt;

/// Why a transport call failed.
///
/// These map to user text one layer up: the I/O thread turns them into a
/// connection state, and the UI never sees a raw OS error.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// The transport was used before [`crate::Transport::open`].
    NotOpen,
    /// The link could not be opened (missing device, busy, permission).
    Open(String),
    /// The link is open but no longer usable.
    Closed,
    /// A write failed part way through.
    Write(String),
    /// A read failed for a reason other than the deadline.
    Read(String),
    /// The deadline passed before the requested bytes arrived.
    Timeout {
        /// Bytes that did arrive; the conversation is not recoverable from them.
        got: usize,
        /// Bytes the caller asked for.
        expected: usize,
    },
    /// The operation does not apply to this transport.
    Unsupported(&'static str),
}

impl fmt::Display for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotOpen => formatter.write_str("the transport is not open"),
            Self::Open(message) => write!(formatter, "could not open the transport: {message}"),
            Self::Closed => formatter.write_str("the transport is closed"),
            Self::Write(message) => write!(formatter, "write failed: {message}"),
            Self::Read(message) => write!(formatter, "read failed: {message}"),
            Self::Timeout { got, expected } => {
                write!(formatter, "timed out after {got} of {expected} byte(s)")
            }
            Self::Unsupported(what) => {
                write!(formatter, "{what} is not supported by this transport")
            }
        }
    }
}

impl std::error::Error for TransportError {}
