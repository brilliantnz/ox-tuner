//! A byte pipe that answers one signature query.
//!
//! The full simulator needs a definition: it builds memory, command patterns
//! and a realtime generator from it. A probe runs before any definition exists,
//! so it gets the smaller thing it needs here: the family's query byte and a
//! signature to answer with. Nothing else in the simulator changes.

use std::collections::VecDeque;
use std::time::Duration;

use oxtuner_protocol::{Frame, LinkMode, ProbeSpec, ReplyCode};
use oxtuner_transport::{Transport, TransportError};

/// A probe-shaped virtual ECU: one query in, one signature out.
#[derive(Debug)]
pub struct SimProbe {
    spec: ProbeSpec,
    signature: String,
    output: VecDeque<u8>,
    open: bool,
    connected: bool,
}

impl SimProbe {
    /// Answers `spec`'s query with the family's default signature.
    #[must_use]
    pub fn new(spec: ProbeSpec) -> Self {
        Self::with_signature(spec, default_signature(&spec))
    }

    /// Answers `spec`'s query with `signature`, for a test that needs an exact
    /// text.
    #[must_use]
    pub fn with_signature(spec: ProbeSpec, signature: impl Into<String>) -> Self {
        Self {
            spec,
            signature: signature.into(),
            output: VecDeque::new(),
            open: false,
            connected: false,
        }
    }

    /// The signature this probe reports.
    #[must_use]
    pub fn signature(&self) -> &str {
        &self.signature
    }

    /// The reply bytes for one correctly addressed query.
    fn reply(&self) -> Result<Vec<u8>, TransportError> {
        let text = self.signature.as_bytes();

        match self.spec.link {
            LinkMode::Legacy => {
                let mut reply = text.to_vec();
                reply.push(0);
                Ok(reply)
            }
            LinkMode::Envelope => {
                let mut payload = Vec::with_capacity(text.len() + 1);
                if self.spec.reply_code == ReplyCode::Leading {
                    payload.push(0);
                }
                payload.extend_from_slice(text);
                Frame::encode(&payload).map_err(|error| TransportError::Write(error.to_string()))
            }
        }
    }

    /// Whether `request` is the family's query, in either framing.
    fn is_query(&self, request: &[u8]) -> bool {
        match self.spec.link {
            LinkMode::Legacy => request == self.spec.query,
            LinkMode::Envelope => match Frame::parse(request) {
                Ok(frame) => frame.payload == self.spec.query,
                Err(_) => false,
            },
        }
    }
}

/// The signature a probe reports when a test does not name one.
fn default_signature(spec: &ProbeSpec) -> String {
    format!("{} synthetic", spec.name)
}

impl Transport for SimProbe {
    fn open(&mut self) -> Result<(), TransportError> {
        self.open = true;
        self.connected = true;
        self.output.clear();
        Ok(())
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        if !self.open {
            return Err(TransportError::NotOpen);
        }
        if !self.connected {
            return Err(TransportError::Closed);
        }

        // Anything that is not the query is answered with silence, which is
        // what a probe reports as "no answer".
        if self.is_query(bytes) {
            let reply = self.reply()?;
            self.output.extend(reply);
        }

        Ok(())
    }

    fn read_exact_timeout(
        &mut self,
        buf: &mut [u8],
        _timeout: Duration,
    ) -> Result<(), TransportError> {
        if !self.open {
            return Err(TransportError::NotOpen);
        }

        let mut filled = 0;
        while filled < buf.len() {
            let Some(byte) = self.output.pop_front() else {
                return Err(TransportError::Timeout {
                    got: filled,
                    expected: buf.len(),
                });
            };
            buf[filled] = byte;
            filled += 1;
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
        while sink.len() - before < max {
            let Some(byte) = self.output.pop_front() else {
                break;
            };
            sink.push(byte);
            if byte == terminator {
                break;
            }
        }

        if sink.len() == before {
            return Err(TransportError::Timeout {
                got: 0,
                expected: 1,
            });
        }
        Ok(())
    }

    fn read_available(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        if !self.open {
            return Err(TransportError::NotOpen);
        }

        let mut read = 0;
        while read < buf.len() {
            let Some(byte) = self.output.pop_front() else {
                break;
            };
            buf[read] = byte;
            read += 1;
        }
        Ok(read)
    }

    fn flush_input(&mut self) {
        self.output.clear();
    }

    fn close(&mut self) {
        self.open = false;
        self.connected = false;
        self.output.clear();
    }
}
