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

/// A probe-shaped link that answers any of several queries over one open port.
///
/// A scan opens a port once and asks every family and framing before releasing
/// it, so the virtual link recognises each query instead of one. A query no
/// probe in the set answers is met with silence, which is what a board that
/// speaks one family does.
#[derive(Debug)]
pub struct SimProbeSet {
    probes: Vec<SimProbe>,
    answering: Option<usize>,
    open: bool,
}

impl SimProbeSet {
    /// One probe per spec, each answering its family's default signature.
    #[must_use]
    pub fn new(specs: impl IntoIterator<Item = ProbeSpec>) -> Self {
        Self::from_probes(specs.into_iter().map(SimProbe::new))
    }

    /// One probe per spec, all silent, like a port with no ECU attached.
    #[must_use]
    pub fn silent(specs: impl IntoIterator<Item = ProbeSpec>) -> Self {
        Self::from_probes(specs.into_iter().map(SimProbe::silent))
    }

    /// One probe per spec, each answering `signature`.
    #[must_use]
    pub fn with_signature(
        specs: impl IntoIterator<Item = ProbeSpec>,
        signature: impl Into<String>,
    ) -> Self {
        let signature = signature.into();

        Self::from_probes(
            specs
                .into_iter()
                .map(|spec| SimProbe::with_signature(spec, signature.clone())),
        )
    }

    /// A set built query by query, so a test can leave the early ones silent.
    #[must_use]
    pub fn from_probes(probes: impl IntoIterator<Item = SimProbe>) -> Self {
        Self {
            probes: probes.into_iter().collect(),
            answering: None,
            open: false,
        }
    }
}

impl Transport for SimProbeSet {
    fn open(&mut self) -> Result<(), TransportError> {
        for probe in &mut self.probes {
            probe.open()?;
        }

        self.answering = None;
        self.open = true;

        Ok(())
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        if !self.open {
            return Err(TransportError::NotOpen);
        }

        for (index, probe) in self.probes.iter_mut().enumerate() {
            if probe.is_query(bytes) {
                probe.write_all(bytes)?;
                self.answering = Some(index);
                return Ok(());
            }
        }

        // A query no probe speaks is met with silence, not an error. The
        // previous answer must not stay selected, or the silence would read from
        // the wrong probe.
        self.answering = None;

        Ok(())
    }

    fn read_exact_timeout(
        &mut self,
        buf: &mut [u8],
        timeout: Duration,
    ) -> Result<(), TransportError> {
        self.reading(buf.len())?.read_exact_timeout(buf, timeout)
    }

    fn read_until(
        &mut self,
        sink: &mut Vec<u8>,
        max: usize,
        terminator: u8,
        quiet: Duration,
        timeout: Duration,
    ) -> Result<(), TransportError> {
        self.reading(1)?
            .read_until(sink, max, terminator, quiet, timeout)
    }

    fn read_available(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        if !self.open {
            return Err(TransportError::NotOpen);
        }

        // Nothing buffered is an answer here, not a timed-out read: that is
        // what a silent port does.
        match self.answering.and_then(|index| self.probes.get_mut(index)) {
            Some(probe) => probe.read_available(buf),
            None => Ok(0),
        }
    }

    fn flush_input(&mut self) {
        for probe in &mut self.probes {
            probe.flush_input();
        }
    }

    fn close(&mut self) {
        for probe in &mut self.probes {
            probe.close();
        }

        self.answering = None;
        self.open = false;
    }
}

impl SimProbeSet {
    /// The probe whose answer is pending, or the silence of the whole set.
    ///
    /// `expected` is what the caller asked for, so a silent read reports the
    /// same shape as a real one that ran out of bytes.
    fn reading(&mut self, expected: usize) -> Result<&mut SimProbe, TransportError> {
        if !self.open {
            return Err(TransportError::NotOpen);
        }

        match self.answering.and_then(|index| self.probes.get_mut(index)) {
            Some(probe) => Ok(probe),
            None => Err(TransportError::Timeout { got: 0, expected }),
        }
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

    /// Everything queued as an answer, read in one go.
    ///
    /// A framed reply starts with a length, so the text terminator is not what
    /// ends it.
    fn drain(set: &mut SimProbeSet) -> Vec<u8> {
        let mut buf = [0u8; 64];
        let read = set.read_available(&mut buf).expect("the answer is ready");

        buf[..read].to_vec()
    }

    #[test]
    fn a_probe_set_answers_each_query_it_was_built_for() {
        let mut set = SimProbeSet::new([
            SPEEDUINO.with_link(LinkMode::Envelope),
            RUSEFI.with_link(LinkMode::Legacy),
        ]);
        set.open().expect("the set opens");

        let query = Frame::encode(b"Q").expect("the query fits a frame");
        set.write_all(&query).expect("the query is written");
        let frame = Frame::parse(&drain(&mut set)).expect("the answer is framed");
        assert!(
            frame.payload.ends_with(b"speeduino synthetic"),
            "the first query is answered by its own family"
        );

        set.write_all(b"S").expect("the second query is written");
        let reply = drain(&mut set);
        assert!(
            reply.starts_with(b"rusefi synthetic"),
            "the second query is answered in its own framing"
        );
    }

    #[test]
    fn a_query_no_probe_speaks_clears_the_previous_answer() {
        let mut set = SimProbeSet::new([SPEEDUINO]);
        set.open().expect("the set opens");

        set.write_all(b"Q").expect("the first query is written");
        set.write_all(b"S").expect("the second query is written");

        let mut buf = [0u8; 64];
        assert!(
            matches!(set.read_available(&mut buf), Ok(0)),
            "an unmatched query leaves nothing to read"
        );
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
