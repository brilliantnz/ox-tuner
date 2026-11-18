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
}

impl SimProbe {
    /// Answers `spec`'s query with the family's default signature.
    #[must_use]
    pub fn new(spec: ProbeSpec) -> Self {
        Self::with_signature(spec, default_signature(&spec))
    }

    /// Answers nothing, like a port with no ECU attached.
    ///
    /// A scan needs the miss path as well as the hit, so a test can drive one
    /// port that stays silent and one that answers.
    #[must_use]
    pub fn silent(spec: ProbeSpec) -> Self {
        Self::with_signature(spec, "")
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
        if text.is_empty() {
            return Ok(Vec::new());
        }

        match self.spec.link {
            LinkMode::Legacy => {
                let mut reply = text.to_vec();
                reply.push(0);
                Ok(reply)
            }
            LinkMode::Envelope => {
                let mut payload = Vec::with_capacity(text.len() + 1);
                if self.spec.behavior.as_dyn().reply_code() == ReplyCode::Leading {
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
    format!("{} synthetic", spec.behavior_id())
}

impl Transport for SimProbe {
    fn open(&mut self) -> Result<(), TransportError> {
        self.open = true;
        self.output.clear();
        Ok(())
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        if !self.open {
            return Err(TransportError::NotOpen);
        }

        // Anything that is not the query is answered with silence, which is
        // what a probe reports as "no answer".
        if self.is_query(bytes) {
            let reply = self.reply()?;
            super::queue::fill(&mut self.output, reply);
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

        super::queue::read_exact(&mut self.output, buf)
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

        super::queue::read_until(&mut self.output, sink, max, terminator)
    }

    fn read_available(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        if !self.open {
            return Err(TransportError::NotOpen);
        }

        Ok(super::queue::read_available(&mut self.output, buf))
    }

    fn flush_input(&mut self) {
        self.output.clear();
    }

    fn close(&mut self) {
        self.open = false;
        self.output.clear();
    }
}

#[cfg(test)]
mod tests {
    use oxtuner_protocol::{RUSEFI, SPEEDUINO};

    use super::*;

    fn probe(spec: ProbeSpec) -> SimProbe {
        let mut probe = SimProbe::new(spec);
        probe.open().expect("the probe opens");
        probe
    }

    #[test]
    fn the_default_signature_classifies_as_the_probe_family() {
        for spec in [SPEEDUINO, RUSEFI] {
            assert_eq!(
                oxtuner_protocol::BehaviorKind::for_signature(&default_signature(&spec)),
                Some(spec.behavior),
                "the simulator must produce a signature the real classifier accepts"
            );
        }
    }

    #[test]
    fn the_legacy_probe_answers_the_query_and_nothing_else() {
        let mut probe = probe(SPEEDUINO);

        probe.write_all(b"Q").expect("the query is written");
        let mut reply = Vec::new();
        probe
            .read_until(&mut reply, 64, 0, Duration::ZERO, Duration::ZERO)
            .expect("the answer arrives");

        let text = String::from_utf8(reply).expect("the reply is text");
        assert_eq!(text, "speeduino synthetic\0");
        assert_eq!(probe.signature(), "speeduino synthetic");
    }

    #[test]
    fn a_query_for_another_family_stays_silent() {
        let mut probe = probe(SPEEDUINO);
        probe.write_all(b"S").expect("the query is written");

        let mut reply = Vec::new();
        let outcome = probe.read_until(&mut reply, 64, 0, Duration::ZERO, Duration::ZERO);

        assert!(matches!(
            outcome,
            Err(TransportError::Timeout { got: 0, .. })
        ));
        assert!(reply.is_empty(), "nothing is answered: {reply:?}");
    }

    #[test]
    fn the_rusefi_probe_answers_with_the_data_alone_when_framed() {
        let spec = RUSEFI.with_link(LinkMode::Envelope);
        let mut probe = probe(spec);

        let request = Frame::encode(b"S").expect("the query frames");
        probe.write_all(&request).expect("the query is written");

        let mut reply = [0u8; 256];
        let read = probe
            .read_available(&mut reply)
            .expect("the answer arrives");
        let reply = &reply[..read];

        let frame = Frame::parse(reply).expect("the answer is one frame");
        assert_eq!(
            String::from_utf8(frame.payload).expect("the payload is text"),
            "rusefi synthetic",
            "a family that sends the data alone carries no leading code"
        );
    }

    #[test]
    fn the_speeduino_probe_leads_a_framed_reply_with_a_return_code() {
        let spec = SPEEDUINO.with_link(LinkMode::Envelope);
        let mut probe = probe(spec);

        let request = Frame::encode(b"Q").expect("the query frames");
        probe.write_all(&request).expect("the query is written");

        let mut reply = [0u8; 256];
        let read = probe
            .read_available(&mut reply)
            .expect("the answer arrives");
        let reply = &reply[..read];

        let frame = Frame::parse(reply).expect("the answer is one frame");
        assert_eq!(frame.payload.first(), Some(&0), "the success code leads");
    }

    #[test]
    fn a_silent_probe_answers_nothing() {
        let mut probe = SimProbe::silent(SPEEDUINO);
        probe.open().expect("the probe opens");

        probe.write_all(b"Q").expect("the query is written");

        let mut buf = [0u8; 16];
        assert_eq!(probe.read_available(&mut buf).expect("no answer"), 0);
    }

    #[test]
    fn a_probe_that_is_not_open_refuses_a_write() {
        let mut probe = SimProbe::new(SPEEDUINO);

        assert!(matches!(
            probe.write_all(b"Q"),
            Err(TransportError::NotOpen)
        ));
    }
}
