//! A transport wrapper that records both directions as a [`Trace`].

use std::time::{Duration, Instant};

use crate::Transport;
use crate::error::TransportError;
use crate::trace::{Direction, Trace, TraceFrame};

/// Records every byte a wrapped transport sends or receives.
///
/// The wrapper is transparent: callers get exactly the wrapped transport's
/// behaviour and errors. Recording cannot fail a conversation; only saving the
/// trace can fail, which is why [`Self::save`] returns a result and the read and
/// write paths do not.
pub struct RecordingTransport {
    inner: Box<dyn Transport>,
    started: Instant,
    frames: Vec<TraceFrame>,
    headers: Vec<String>,
}

impl RecordingTransport {
    /// Wraps `inner` and starts the clock.
    pub fn new(inner: Box<dyn Transport>) -> Self {
        Self {
            inner,
            started: Instant::now(),
            frames: Vec::new(),
            headers: vec!["# ox-tuner trace v1".to_owned()],
        }
    }

    /// Adds a header note (port, definition signature, firmware).
    pub fn note(&mut self, note: impl Into<String>) {
        let note = note.into();
        // One note per line keeps the file parseable and readable.
        for line in note.lines() {
            self.headers.push(format!("# {line}"));
        }
    }

    /// The frames recorded so far.
    pub fn frames(&self) -> &[TraceFrame] {
        &self.frames
    }

    /// The trace recorded so far, owned.
    pub fn trace(&self) -> Trace {
        Trace::from_parts(self.frames.clone(), self.headers.clone())
    }

    /// Saves the trace recorded so far and returns its frame count.
    pub fn save(&self, path: &std::path::Path) -> Result<usize, TransportError> {
        let trace = self.trace();
        trace.save(path)?;
        Ok(trace.len())
    }

    /// Number of frames recorded so far.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Whether nothing has crossed the wire yet.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    fn record(&mut self, direction: Direction, bytes: &[u8]) {
        // An empty write/read is not a frame: a trace should not gain lines for
        // operations that moved no bytes.
        if bytes.is_empty() {
            return;
        }
        self.frames.push(TraceFrame::new(
            direction,
            bytes.to_vec(),
            self.started.elapsed(),
        ));
    }
}

impl Transport for RecordingTransport {
    fn open(&mut self) -> Result<(), TransportError> {
        self.inner.open()
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.inner.write_all(bytes)?;
        self.record(Direction::Tx, bytes);
        Ok(())
    }

    fn read_exact_timeout(
        &mut self,
        buf: &mut [u8],
        timeout: Duration,
    ) -> Result<(), TransportError> {
        let result = self.inner.read_exact_timeout(buf, timeout);
        // Record what actually arrived, even when the read times out: a
        // partial reply is exactly what a replay must reproduce.
        let filled = match &result {
            Ok(()) => buf.len(),
            Err(TransportError::Timeout { got, .. }) => (*got).min(buf.len()),
            Err(_) => 0,
        };
        self.record(Direction::Rx, &buf[..filled]);
        result
    }

    fn read_until(
        &mut self,
        sink: &mut Vec<u8>,
        max: usize,
        terminator: u8,
        quiet: Duration,
        timeout: Duration,
    ) -> Result<(), TransportError> {
        let before = sink.len();
        let result = self.inner.read_until(sink, max, terminator, quiet, timeout);
        let received = sink[before..].to_vec();
        self.record(Direction::Rx, &received);
        result
    }

    fn read_available(&mut self, buf: &mut [u8]) -> Result<usize, TransportError> {
        let read = self.inner.read_available(buf)?;
        self.record(Direction::Rx, &buf[..read]);
        Ok(read)
    }

    fn flush_input(&mut self) {
        self.inner.flush_input();
    }

    fn close(&mut self) {
        self.inner.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ReplayTransport;

    #[test]
    fn a_recorded_conversation_replays_byte_for_byte() {
        let trace = Trace::parse("TX 0 51\nRX 0 73 70 65 65 64\n").expect("a trace");
        let replay = ReplayTransport::new(trace.clone());

        let mut recorder = RecordingTransport::new(Box::new(replay));
        recorder.open().expect("opens");
        recorder.write_all(&[0x51]).expect("writes");
        let mut reply = Vec::new();
        recorder
            .read_until(
                &mut reply,
                32,
                b'\n',
                Duration::from_millis(1),
                Duration::from_secs(1),
            )
            .expect("reads");

        let recorded = recorder.trace();
        assert_eq!(recorded.frames().len(), 2);
        assert_eq!(recorded.frames()[0].bytes, vec![0x51]);
        assert_eq!(recorded.frames()[1].bytes, b"speed");
    }
}
