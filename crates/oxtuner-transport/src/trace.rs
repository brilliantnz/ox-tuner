//! Session traces: both directions of a conversation, on disk.
//!
//! A trace is the byte evidence of a session — what was sent, what came back,
//! and when. Hardware runs produce them ([`RecordingTransport`]), CI replays
//! them ([`crate::ReplayTransport`]), and the simulator's determinism tests can
//! compare one against another (ADR-0012).
//!
//! # Format
//!
//! ```text
//! # ox-tuner trace v1
//! # signature: speeduino 202501
//! TX 0 51
//! RX 3 73 70 65 65 64 75 69 6e 6f 20 32 30 32 35 30 31
//! ```
//!
//! One frame per line: direction (`TX`/`RX`), milliseconds since the first
//! frame, then the bytes as hex. The header comments are free-form and ignored
//! by the parser, so a capture can carry the port, firmware and INI versions
//! without changing the format.

use std::fmt::Write as _;
use std::time::Duration;

use crate::error::TransportError;

/// Which way a recorded frame travelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Host → ECU.
    Tx,
    /// ECU → host.
    Rx,
}

/// One recorded frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceFrame {
    /// Direction of travel.
    pub direction: Direction,
    /// Bytes exactly as they crossed the wire.
    pub bytes: Vec<u8>,
    /// Time since the trace started.
    pub at: Duration,
}

impl TraceFrame {
    /// Builds a frame at a given offset.
    pub fn new(direction: Direction, bytes: impl Into<Vec<u8>>, at: Duration) -> Self {
        Self {
            direction,
            bytes: bytes.into(),
            at,
        }
    }
}

/// A recorded session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Trace {
    frames: Vec<TraceFrame>,
    /// Free-form header lines, kept so a saved trace can be re-saved unchanged.
    headers: Vec<String>,
}

/// Why a trace could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceError {
    /// One-based line number.
    pub line: usize,
    /// What was wrong with it.
    pub message: String,
}

impl std::fmt::Display for TraceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "trace line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for TraceError {}

impl Trace {
    /// An empty trace with the format header.
    pub fn new() -> Self {
        Self {
            frames: Vec::new(),
            headers: vec!["# ox-tuner trace v1".to_owned()],
        }
    }

    /// Builds a trace from frames, with the format header.
    pub fn from_frames(frames: Vec<TraceFrame>) -> Self {
        Self {
            frames,
            headers: vec!["# ox-tuner trace v1".to_owned()],
        }
    }

    /// Builds a trace from frames and header lines.
    pub(crate) fn from_parts(frames: Vec<TraceFrame>, headers: Vec<String>) -> Self {
        Self { frames, headers }
    }

    /// Parses the text format.
    ///
    /// Comments and blank lines are kept as headers, so `parse` followed by
    /// `to_string` preserves the notes that make a trace useful.
    pub fn parse(text: &str) -> Result<Self, TraceError> {
        let mut trace = Trace {
            frames: Vec::new(),
            headers: Vec::new(),
        };

        for (index, line) in text.lines().enumerate() {
            let number = index + 1;
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('#') {
                trace.headers.push(line.to_owned());
                continue;
            }

            let mut fields = line.split_whitespace();
            let direction = match fields.next() {
                Some("TX") => Direction::Tx,
                Some("RX") => Direction::Rx,
                Some(other) => {
                    return Err(TraceError {
                        line: number,
                        message: format!("expected TX or RX, found {other:?}"),
                    });
                }
                None => continue,
            };

            let at = match fields.next() {
                Some(value) => value
                    .parse::<u64>()
                    .map(Duration::from_millis)
                    .map_err(|_| TraceError {
                        line: number,
                        message: format!("{value:?} is not a millisecond offset"),
                    })?,
                None => {
                    return Err(TraceError {
                        line: number,
                        message: "the frame has no millisecond offset".to_owned(),
                    });
                }
            };

            let mut bytes = Vec::new();
            for field in fields {
                let byte = u8::from_str_radix(field, 16).map_err(|_| TraceError {
                    line: number,
                    message: format!("{field:?} is not a hex byte"),
                })?;
                bytes.push(byte);
            }
            if bytes.is_empty() {
                return Err(TraceError {
                    line: number,
                    message: "the frame has no bytes".to_owned(),
                });
            }
            trace.frames.push(TraceFrame {
                direction,
                bytes,
                at,
            });
        }

        Ok(trace)
    }

    /// The frames, in order.
    pub fn frames(&self) -> &[TraceFrame] {
        &self.frames
    }

    /// The header lines, including the format marker.
    pub fn headers(&self) -> &[String] {
        &self.headers
    }

    /// Adds a header note, e.g. the port and firmware.
    pub fn note(&mut self, note: impl Into<String>) {
        self.headers.push(format!("# {}", note.into()));
    }

    /// Number of frames.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Whether nothing was recorded.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Appends a frame measured from the trace's own start.
    pub fn push(&mut self, frame: TraceFrame) {
        self.frames.push(frame);
    }

    /// The text form, deterministic and stable.
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        for header in &self.headers {
            let _ = writeln!(text, "{header}");
        }
        for frame in &self.frames {
            let direction = match frame.direction {
                Direction::Tx => "TX",
                Direction::Rx => "RX",
            };
            let _ = write!(text, "{direction} {}", frame.at.as_millis());
            for byte in &frame.bytes {
                let _ = write!(text, " {byte:02x}");
            }
            text.push('\n');
        }
        text
    }

    /// Saves the trace with an atomic replace, so a crash cannot leave half a
    /// file where a replayable trace used to be.
    pub fn save(&self, path: &std::path::Path) -> Result<(), TransportError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| TransportError::Write(format!("{}: {error}", parent.display())))?;
        }
        let temporary = path.with_extension("trace.tmp");
        std::fs::write(&temporary, self.to_text())
            .map_err(|error| TransportError::Write(format!("{}: {error}", temporary.display())))?;
        std::fs::rename(&temporary, path)
            .map_err(|error| TransportError::Write(format!("{}: {error}", path.display())))?;
        Ok(())
    }

    /// Loads a trace from disk.
    pub fn load(path: &std::path::Path) -> Result<Self, TransportError> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| TransportError::Read(format!("{}: {error}", path.display())))?;
        Self::parse(&text)
            .map_err(|error| TransportError::Read(format!("{}: {error}", path.display())))
    }
}

impl std::fmt::Display for Trace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.to_text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_trace_round_trips_through_its_text_form() {
        let mut trace = Trace::new();
        trace.note("signature: speeduino 202501");
        trace.push(TraceFrame::new(Direction::Tx, vec![0x51], Duration::ZERO));
        trace.push(TraceFrame::new(
            Direction::Rx,
            b"speeduino 202501".to_vec(),
            Duration::from_millis(3),
        ));

        let parsed = Trace::parse(&trace.to_text()).expect("a saved trace parses");

        assert_eq!(parsed.frames(), trace.frames());
        assert!(
            parsed
                .headers()
                .iter()
                .any(|line| line.contains("signature"))
        );
    }

    #[test]
    fn a_bad_byte_names_its_line() {
        let error = Trace::parse("TX 0 zz").expect_err("not hex");

        assert_eq!(error.line, 1);
        assert!(error.message.contains("zz"));
    }

    #[test]
    fn a_trace_with_no_frames_is_empty() {
        let trace = Trace::parse("# ox-tuner trace v1\n").expect("headers only");

        assert!(trace.is_empty());
        assert_eq!(trace.len(), 0);
    }
}
