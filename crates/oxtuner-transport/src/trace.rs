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

/// The largest single frame a trace may contain.
///
/// A page chunk is far smaller; the cap keeps one corrupt or hand-written line
/// from allocating an unbounded vector.
pub(crate) const MAX_FRAME_BYTES: usize = 1 << 20;

/// The largest trace this build loads, in bytes on disk.
pub(crate) const MAX_TRACE_BYTES: usize = 64 << 20;

/// The most frames a trace may hold.
///
/// A 20 Hz session records roughly 18 000 exchanges an hour; 250 000 covers a
/// long bench run and still bounds memory.
pub(crate) const MAX_TRACE_FRAMES: usize = 250_000;

/// Which way a recorded frame travelled.
#[expect(
    clippy::exhaustive_enums,
    reason = "TX and RX are the complete set of directions a frame can travel"
)]
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
    ///
    /// # Errors
    ///
    /// Returns [`TraceError`] naming the line when the frame size, byte count
    /// or frame count limits are exceeded or a line is malformed.
    pub fn parse(text: &str) -> Result<Self, TraceError> {
        if text.len() > MAX_TRACE_BYTES {
            return Err(TraceError {
                line: 0,
                message: format!(
                    "the trace is {} bytes, the limit is {MAX_TRACE_BYTES}",
                    text.len()
                ),
            });
        }
        let mut trace = Trace {
            frames: Vec::new(),
            headers: Vec::new(),
        };
        let mut total_bytes = 0usize;

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
            // Three characters per hex byte plus the direction and offset.
            if line.len() > MAX_FRAME_BYTES.saturating_mul(3).saturating_add(32) {
                return Err(TraceError {
                    line: number,
                    message: "the frame line is longer than any frame may be".to_owned(),
                });
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
                if bytes.len() >= MAX_FRAME_BYTES {
                    return Err(TraceError {
                        line: number,
                        message: format!("the frame exceeds {MAX_FRAME_BYTES} bytes"),
                    });
                }
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
            total_bytes = total_bytes.saturating_add(bytes.len());
            if total_bytes > MAX_TRACE_BYTES {
                return Err(TraceError {
                    line: number,
                    message: format!("the trace exceeds {MAX_TRACE_BYTES} bytes"),
                });
            }
            if trace.frames.len() >= MAX_TRACE_FRAMES {
                return Err(TraceError {
                    line: number,
                    message: format!("the trace exceeds {MAX_TRACE_FRAMES} frames"),
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
        text(&self.headers, &self.frames)
    }

    /// Saves the trace with an atomic replace, so a crash cannot leave half a
    /// file where a replayable trace used to be.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Write`] when the temporary file or the final
    /// rename cannot be written.
    pub fn save(&self, path: &std::path::Path) -> Result<(), TransportError> {
        write(path, &self.headers, &self.frames)
    }

    /// Loads a trace from disk.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Read`] when the file cannot be read, exceeds
    /// the trace size limit, or does not parse.
    pub fn load(path: &std::path::Path) -> Result<Self, TransportError> {
        let metadata = std::fs::metadata(path)
            .map_err(|error| TransportError::Read(format!("{}: {error}", path.display())))?;
        let limit = u64::try_from(MAX_TRACE_BYTES).unwrap_or(u64::MAX);
        if metadata.len() > limit {
            return Err(TransportError::Read(format!(
                "{}: the trace is {} bytes, the limit is {MAX_TRACE_BYTES}",
                path.display(),
                metadata.len()
            )));
        }
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

/// Writes a trace from borrowed parts, with an atomic replace.
///
/// Shared with [`crate::RecordingTransport`], whose frame list must not be
/// cloned just to save it.
pub(crate) fn write(
    path: &std::path::Path,
    headers: &[String],
    frames: &[TraceFrame],
) -> Result<(), TransportError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| TransportError::Write(format!("{}: {error}", parent.display())))?;
    }
    let temporary = path.with_extension("trace.tmp");
    std::fs::write(&temporary, text(headers, frames))
        .map_err(|error| TransportError::Write(format!("{}: {error}", temporary.display())))?;
    std::fs::rename(&temporary, path)
        .map_err(|error| TransportError::Write(format!("{}: {error}", path.display())))?;
    Ok(())
}

/// The trace's text form for borrowed parts.
pub(crate) fn text(headers: &[String], frames: &[TraceFrame]) -> String {
    let mut body = String::new();
    for header in headers {
        let _ = writeln!(body, "{header}");
    }
    for frame in frames {
        let direction = match frame.direction {
            Direction::Tx => "TX",
            Direction::Rx => "RX",
        };
        let _ = write!(body, "{direction} {}", frame.at.as_millis());
        for byte in &frame.bytes {
            let _ = write!(body, " {byte:02x}");
        }
        body.push('\n');
    }
    body
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
    fn a_frame_line_larger_than_one_frame_is_rejected() {
        let mut text = String::from("RX 0 ");
        text.push_str(&"00 ".repeat(MAX_FRAME_BYTES + 100));

        let error = Trace::parse(&text).expect_err("the line is too large");

        assert!(error.message.contains("longer"), "{}", error.message);
    }

    #[test]
    fn a_trace_file_larger_than_the_limit_is_not_read() {
        let path = std::env::temp_dir().join(format!(
            "oxtuner-trace-limit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos())
        ));
        let file = std::fs::File::create(&path).expect("creates");
        // Sparse: no 64 MiB of RAM or disk pages are actually written.
        let oversized = u64::try_from(MAX_TRACE_BYTES + 1).expect("the trace limit fits in u64");
        file.set_len(oversized).expect("sizes");

        let error = Trace::load(&path).expect_err("the file is too large");
        let _ = std::fs::remove_file(&path);

        assert!(error.to_string().contains("limit"), "{error}");
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
