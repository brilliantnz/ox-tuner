//! Byte transports: the wire, and nothing above it.
//!
//! A [`Transport`] moves bytes. It knows no protocol, no page, no signature and
//! no tune — that separation is what lets the same conversation run over serial
//! hardware, the simulator, or a recorded trace (ADR-0008).
//!
//! ```text
//! Protocol ──▶ trait Transport ──┬── SerialTransport  (the real port)
//!                                ├── SimTransport     (oxtuner-sim)
//!                                ├── RecordingTransport (wraps any, writes a trace)
//!                                └── ReplayTransport  (a trace, as a transport)
//! ```
//!
//! # One owner
//!
//! A transport is used by exactly one thread — the core's I/O thread
//! (ADR-0005/0008). The trait is `Send` because the instance moves to that
//! thread when it starts, never shared between two.
//!
//! # Timeouts
//!
//! Every read takes its own deadline. [`Transport::read_exact_timeout`] fills
//! the whole buffer or fails with [`TransportError::Timeout`]; a partial reply
//! is therefore never mistaken for a complete one. [`Transport::read_until`]
//! exists for replies the firmware does not frame (Speeduino answers the
//! signature query with text and no terminator), so it also accepts a quiet
//! period after at least one byte.
//!
//! # Example
//!
//! ```
//! use oxtuner_transport::{ReplayTransport, Trace, Transport};
//! use std::time::Duration;
//!
//! let trace = Trace::parse("TX 0 51\nRX 0 73 70 65 65 64\n").expect("a trace");
//! let mut transport = ReplayTransport::new(trace);
//! transport.open().expect("opens");
//!
//! transport.write_all(&[0x51]).expect("writes");
//! let mut reply = Vec::new();
//! transport
//!     .read_until(&mut reply, 32, 0, Duration::from_millis(1), Duration::from_secs(1))
//!     .expect("reads");
//! assert_eq!(reply, b"speed");
//! ```

#![forbid(unsafe_code)]

mod error;
mod recording;
mod replay;
mod serial;
mod trace;

pub use error::TransportError;
pub use recording::RecordingTransport;
pub use replay::ReplayTransport;
pub use serial::{PortInfo, SerialSettings, SerialTransport, ports};
pub use trace::{Direction, Trace, TraceError, TraceFrame};

/// A byte transport.
///
/// Implementations are blocking and own their file descriptor (or virtual
/// queue). Calls are strictly sequential, matching the ECU's own rule that a
/// new command must not follow before the previous reply is complete
/// (ADR-0008).
pub trait Transport: Send {
    /// Opens the link. Opening an already-open transport is a no-op, so a
    /// reconnect can call it unconditionally.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Open`] when the device is missing, busy or
    /// not permitted.
    fn open(&mut self) -> Result<(), TransportError>;

    /// Writes every byte. A short write is an error, never silently retried.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::NotOpen`] before the link is open and
    /// [`TransportError::Write`] when the device rejects the bytes.
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), TransportError>;

    /// Fills `buf` completely, or fails.
    ///
    /// [`TransportError::Timeout`] carries how many bytes arrived before the
    /// deadline; callers must treat the buffer as partially filled and discard
    /// the conversation.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Timeout`], [`TransportError::Read`] or
    /// [`TransportError::Closed`] when the reply cannot be completed.
    fn read_exact_timeout(
        &mut self,
        buf: &mut [u8],
        timeout: std::time::Duration,
    ) -> Result<(), TransportError>;

    /// Reads until `terminator` (inclusive), `max` bytes, or a quiet period
    /// after at least one byte.
    ///
    /// The quiet period exists for unframed text replies: Speeduino answers the
    /// signature query with a plain string and no terminator, so "no more bytes
    /// for a while" is the only end marker it offers.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Timeout`] when not one byte arrives before the
    /// deadline, and the same read errors as [`Self::read_exact_timeout`].
    fn read_until(
        &mut self,
        sink: &mut Vec<u8>,
        max: usize,
        terminator: u8,
        quiet: std::time::Duration,
        timeout: std::time::Duration,
    ) -> Result<(), TransportError>;

    /// Reads what is already buffered, without waiting. Returns `0` when
    /// nothing is available.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::NotOpen`] before the link is open and
    /// [`TransportError::Read`] when the device cannot be read.
    fn read_available(&mut self, buf: &mut [u8]) -> Result<usize, TransportError>;

    /// Discards anything pending in the input buffer.
    fn flush_input(&mut self);

    /// Closes the link. Calling it on a closed transport is a no-op.
    fn close(&mut self);
}

/// The quiet window [`Transport::read_until`] waits for when a reply has no
/// terminator.
///
/// Chosen against the real ECU: Speeduino's signature arrives as one burst, and
/// 50 ms is far longer than the gap between bytes at 115200 baud (0.09 ms) yet
/// short enough that a connect does not feel slow.
pub const READ_UNTIL_QUIET: std::time::Duration = std::time::Duration::from_millis(50);

impl<T: Transport + ?Sized> Transport for Box<T> {
    fn open(&mut self) -> Result<(), TransportError> {
        (**self).open()
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        (**self).write_all(bytes)
    }

    fn read_exact_timeout(
        &mut self,
        buf: &mut [u8],
        timeout: std::time::Duration,
    ) -> Result<(), TransportError> {
        (**self).read_exact_timeout(buf, timeout)
    }

    fn read_until(
        &mut self,
        sink: &mut Vec<u8>,
        max: usize,
        terminator: u8,
        quiet: std::time::Duration,
        timeout: std::time::Duration,
    ) -> Result<(), TransportError> {
        (**self).read_until(sink, max, terminator, quiet, timeout)
    }

    fn read_available(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        (**self).read_available(buf)
    }

    fn flush_input(&mut self) {
        (**self).flush_input();
    }

    fn close(&mut self) {
        (**self).close();
    }
}
