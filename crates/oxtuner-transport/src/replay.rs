//! A [`Transport`] that answers from a recorded [`Trace`].
//!
//! Replay is the hardware-evidence path in CI: a session recorded on a real ECU
//! runs again through the same protocol code, without the ECU (ADR-0012).
//!
//! Host writes are validated against the next recorded TX frame, so the replay
//! proves the whole conversation, not only the replies. Reads are permissive:
//! a TX frame no write matched is skipped rather than turned into a fake reply.

use std::collections::VecDeque;
use std::time::Duration;

use crate::Transport;
use crate::error::TransportError;
use crate::trace::{Direction, Trace};

/// Replays a recorded trace.
///
/// Frames are consumed in order. The transport ignores wall-clock timing by
/// default, so a CI replay is fast and deterministic; [`Self::with_timing`]
/// spaces replies by their recorded offsets when a test needs the real cadence.
#[derive(Debug)]
pub struct ReplayTransport {
    frames: VecDeque<crate::trace::TraceFrame>,
    pending: VecDeque<u8>,
    open: bool,
    started: Option<std::time::Instant>,
    timing: bool,
}

impl ReplayTransport {
    /// Replays `trace`.
    pub fn new(trace: Trace) -> Self {
        Self {
            frames: trace.into_frames().into(),
            pending: VecDeque::new(),
            open: false,
            started: None,
            timing: false,
        }
    }

    /// Spaces replies by their recorded offsets.
    #[must_use]
    pub fn with_timing(mut self) -> Self {
        self.timing = true;
        self
    }

    /// How many frames are left to consume.
    pub fn remaining(&self) -> usize {
        self.frames.len()
    }

    /// The next unwritten frame, if the trace has one.
    fn next_frame(&mut self) -> Option<crate::trace::TraceFrame> {
        self.frames.pop_front()
    }

    /// Whether anything is queued for the host to read.
    fn fill(&mut self) {
        while self.pending.is_empty() {
            let Some(frame) = self.next_frame() else {
                return;
            };
            match frame.direction {
                Direction::Rx => {
                    if self.timing {
                        let started = *self.started.get_or_insert_with(std::time::Instant::now);
                        let due = started + frame.at;
                        if let Some(wait) = due.checked_duration_since(std::time::Instant::now()) {
                            std::thread::sleep(wait);
                        }
                    }
                    self.pending.extend(frame.bytes);
                    return;
                }
                // A TX frame still here was never matched by a write; the
                // caller either skipped that command or the trace omits its
                // request. Skip it rather than inventing a reply.
                Direction::Tx => {}
            }
        }
    }
}

impl Transport for ReplayTransport {
    fn open(&mut self) -> Result<(), TransportError> {
        self.open = true;
        Ok(())
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        if !self.open {
            return Err(TransportError::NotOpen);
        }
        // A trace that records the host's write lets a replay prove the
        // conversation too, not just the replies. A trace without that frame
        // stays permissive, because a hand-written reply fixture need not
        // carry the request.
        match self.frames.front() {
            Some(frame) if frame.direction == Direction::Tx => {
                if frame.bytes == bytes {
                    self.frames.pop_front();
                    Ok(())
                } else {
                    Err(TransportError::Write(format!(
                        "the replay expected the host to write {:02x?}, got {:02x?}",
                        &frame.bytes[..frame.bytes.len().min(8)],
                        &bytes[..bytes.len().min(8)]
                    )))
                }
            }
            _ => Ok(()),
        }
    }

    fn read_exact_timeout(
        &mut self,
        buf: &mut [u8],
        _timeout: Duration,
    ) -> Result<(), TransportError> {
        if !self.open {
            return Err(TransportError::NotOpen);
        }
        self.fill();
        let mut filled = 0;
        while filled < buf.len() {
            if let Some(byte) = self.pending.pop_front() {
                buf[filled] = byte;
                filled += 1;
            } else {
                self.fill();
                if self.pending.is_empty() {
                    return Err(TransportError::Timeout {
                        got: filled,
                        expected: buf.len(),
                    });
                }
            }
        }
        Ok(())
    }

    fn read_until(
        &mut self,
        sink: &mut Vec<u8>,
        max: usize,
        terminator: u8,
        _quiet: Duration,
        _timeout: Duration,
    ) -> Result<(), TransportError> {
        if !self.open {
            return Err(TransportError::NotOpen);
        }
        let before = sink.len();
        loop {
            if sink.len() - before >= max {
                return Ok(());
            }
            if self.pending.is_empty() {
                self.fill();
                if self.pending.is_empty() {
                    return if sink.len() == before {
                        Err(TransportError::Timeout {
                            got: 0,
                            expected: 1,
                        })
                    } else {
                        Ok(())
                    };
                }
            }
            let Some(byte) = self.pending.pop_front() else {
                continue;
            };
            sink.push(byte);
            if byte == terminator {
                return Ok(());
            }
        }
    }

    fn read_available(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        if !self.open {
            return Err(TransportError::NotOpen);
        }
        self.fill();
        let mut read = 0;
        while read < buf.len() {
            let Some(byte) = self.pending.pop_front() else {
                break;
            };
            buf[read] = byte;
            read += 1;
        }
        Ok(read)
    }

    fn flush_input(&mut self) {
        self.pending.clear();
    }

    fn close(&mut self) {
        self.open = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace() -> Trace {
        Trace::parse("TX 0 51\nRX 2 73 70 65 65 64 75 69 6e 6f\n").expect("a trace")
    }

    #[test]
    fn replay_answers_the_frames_in_order() {
        let mut transport = ReplayTransport::new(trace());
        transport.open().expect("opens");
        transport.write_all(&[0x51]).expect("writes");

        let mut reply = vec![0u8; 9];
        transport
            .read_exact_timeout(&mut reply, Duration::from_millis(10))
            .expect("reads");

        assert_eq!(&reply, b"speeduino");
    }

    #[test]
    fn a_reply_that_was_never_recorded_times_out() {
        let mut transport =
            ReplayTransport::new(Trace::parse("TX 0 51\n").expect("a write-only trace"));
        transport.open().expect("opens");
        transport.write_all(&[0x51]).expect("writes");

        let mut reply = vec![0u8; 4];
        let error = transport
            .read_exact_timeout(&mut reply, Duration::from_millis(10))
            .expect_err("no reply was recorded");

        assert!(matches!(
            error,
            TransportError::Timeout {
                got: 0,
                expected: 4
            }
        ));
    }

    #[test]
    fn a_write_that_does_not_match_the_trace_is_reported() {
        let mut transport = ReplayTransport::new(trace());
        transport.open().expect("opens");

        let error = transport.write_all(&[0x52]).expect_err("wrong command");

        assert!(matches!(error, TransportError::Write(_)), "{error}");
    }
}
