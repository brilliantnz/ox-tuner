//! The `msEnvelope_1.0` frame: length, payload, CRC-32.
//!
//! Speeduino's framed protocol wraps the same command bytes the legacy path
//! sends. Both directions use:
//!
//! ```text
//! [length high][length low][payload …][crc32 high][crc32 mid][crc32 mid][crc32 low]
//! ```
//!
//! The length counts payload bytes only; the CRC covers the payload only and is
//! sent most significant byte first. The firmware writes it with
//! `reverse_bytes()` then a raw little-endian write, which is the same bytes.

use crate::crc32::crc32;
use crate::error::ProtocolError;

/// The largest frame this build accepts.
///
/// A Speeduino's serial buffer is 257 bytes, and a page read adds the return
/// code and the frame overhead on top; 4 KiB leaves room for a larger board
/// without letting a corrupt length field ask for a huge allocation.
pub(crate) const MAX_PAYLOAD: usize = 4096;

/// A validated frame payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// The bytes between the length and the CRC.
    pub payload: Vec<u8>,
}

/// Why a byte slice is not a frame.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// Fewer bytes than the smallest possible frame.
    Short {
        /// Bytes available.
        got: usize,
        /// Bytes needed for the header and CRC.
        expected: usize,
    },
    /// The length field and the actual slice disagree.
    Length {
        /// What the length field says.
        declared: usize,
        /// What the slice carries.
        actual: usize,
    },
    /// The payload is larger than the maximum frame payload.
    TooLarge {
        /// The length field's value.
        declared: usize,
    },
    /// The CRC did not match.
    Crc {
        /// The value carried by the frame.
        carried: u32,
        /// The value computed from the payload.
        computed: u32,
    },
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Short { got, expected } => {
                write!(formatter, "{got} byte(s) where a frame needs {expected}")
            }
            Self::Length { declared, actual } => write!(
                formatter,
                "the frame declares {declared} payload byte(s) but carries {actual}"
            ),
            Self::TooLarge { declared } => {
                write!(formatter, "the frame declares {declared} payload bytes")
            }
            Self::Crc { carried, computed } => write!(
                formatter,
                "the frame carries CRC 0x{carried:08x}, the payload computes 0x{computed:08x}"
            ),
        }
    }
}

impl std::error::Error for FrameError {}

impl From<FrameError> for ProtocolError {
    fn from(error: FrameError) -> Self {
        Self::BadFrame(error)
    }
}

impl Frame {
    /// Wraps `payload` in the envelope.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError::TooLarge`] when the payload exceeds
    /// [`MAX_PAYLOAD`].
    pub fn encode(payload: &[u8]) -> Result<Vec<u8>, FrameError> {
        if payload.len() > MAX_PAYLOAD {
            return Err(FrameError::TooLarge {
                declared: payload.len(),
            });
        }
        let length = u16::try_from(payload.len()).map_err(|_| FrameError::TooLarge {
            declared: payload.len(),
        })?;
        let crc = crc32(payload);
        let mut bytes = Vec::with_capacity(payload.len() + 6);
        bytes.extend_from_slice(&length.to_be_bytes());
        bytes.extend_from_slice(payload);
        bytes.extend_from_slice(&crc.to_be_bytes());
        Ok(bytes)
    }

    /// Parses a whole frame, including its length header.
    ///
    /// # Errors
    ///
    /// Returns a [`FrameError`] for a short, oversized, mismatched or
    /// checksum-invalid frame.
    pub fn parse(bytes: &[u8]) -> Result<Self, FrameError> {
        if bytes.len() < 6 {
            return Err(FrameError::Short {
                got: bytes.len(),
                expected: 6,
            });
        }
        let declared = usize::from(u16::from_be_bytes([bytes[0], bytes[1]]));
        if declared > MAX_PAYLOAD {
            return Err(FrameError::TooLarge { declared });
        }
        let expected = declared + 6;
        if bytes.len() != expected {
            return Err(FrameError::Length {
                declared,
                actual: bytes.len().saturating_sub(6),
            });
        }

        let payload = &bytes[2..2 + declared];
        let carried = u32::from_be_bytes([
            bytes[2 + declared],
            bytes[3 + declared],
            bytes[4 + declared],
            bytes[5 + declared],
        ]);
        let computed = crc32(payload);
        if carried != computed {
            return Err(FrameError::Crc { carried, computed });
        }
        Ok(Self {
            payload: payload.to_vec(),
        })
    }

    /// Reads the payload length from a frame's first two bytes.
    pub fn length(prefix: [u8; 2]) -> usize {
        usize::from(u16::from_be_bytes(prefix))
    }
}

/// Assembles frames from a byte stream.
///
/// The protocol normally reads a known number of bytes (the header, then the
/// rest), so the decoder's job is to make a replay or a streamed reply equally
/// safe: garbage never panics and a bad CRC is reported, not ignored.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buffer: Vec<u8>,
}

impl FrameDecoder {
    /// An empty decoder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends bytes and returns every complete frame they finished.
    ///
    /// A decoder that sees an invalid length, a bad CRC or more buffered bytes
    /// than any frame can hold **resets itself** and returns the error. A
    /// caller that keeps feeding a wedged buffer would otherwise re-hit the
    /// same bad bytes forever; after a reset, the next `push` starts clean.
    ///
    /// # Errors
    ///
    /// Returns a [`FrameError`] when the bytes contain an invalid frame.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Frame>, FrameError> {
        // One call may carry at most one frame's worth of bytes. A stream is
        // fed in pieces anyway, and this keeps a single huge slice from being
        // copied into the buffer before the first length check.
        if bytes.len() > MAX_PAYLOAD + 6 {
            return Err(FrameError::TooLarge {
                declared: bytes.len(),
            });
        }
        self.buffer.extend_from_slice(bytes);
        let mut frames = Vec::new();

        loop {
            if self.buffer.len() < 2 {
                break;
            }
            let declared = Frame::length([self.buffer[0], self.buffer[1]]);
            if declared > MAX_PAYLOAD {
                self.buffer.clear();
                return Err(FrameError::TooLarge { declared });
            }
            let total = declared + 6;
            if self.buffer.len() < total {
                // `declared` is within the cap, so the partial frame is
                // bounded by `MAX_PAYLOAD + 6`; wait for the rest.
                break;
            }
            match Frame::parse(&self.buffer[..total]) {
                Ok(frame) => {
                    self.buffer.drain(..total);
                    frames.push(frame);
                }
                Err(error) => {
                    self.buffer.clear();
                    return Err(error);
                }
            }
        }

        Ok(frames)
    }

    /// Bytes waiting for the rest of a frame.
    pub fn pending(&self) -> usize {
        self.buffer.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_round_trips() {
        let encoded = Frame::encode(b"hello").expect("encodes");
        assert_eq!(&encoded[..2], &[0x00, 0x05]);
        assert_eq!(&encoded[2..7], b"hello");

        let frame = Frame::parse(&encoded).expect("parses");
        assert_eq!(frame.payload, b"hello");
    }

    #[test]
    fn a_corrupt_payload_is_rejected() {
        let mut encoded = Frame::encode(b"hello").expect("encodes");
        encoded[3] ^= 0xFF;

        assert!(matches!(
            Frame::parse(&encoded),
            Err(FrameError::Crc { .. })
        ));
    }

    #[test]
    fn a_length_that_does_not_match_the_bytes_is_rejected() {
        let mut encoded = Frame::encode(b"hello").expect("encodes");
        encoded.push(0);

        assert!(matches!(
            Frame::parse(&encoded),
            Err(FrameError::Length { .. })
        ));
    }

    #[test]
    fn a_decoder_that_sees_garbage_resets_instead_of_staying_poisoned() {
        let mut decoder = FrameDecoder::new();
        // A corrupt length field is rejected and the bad bytes are dropped.
        assert!(decoder.push(&[0xFF, 0xFF]).is_err());
        assert_eq!(decoder.pending(), 0);

        let encoded = Frame::encode(b"ok").expect("encodes");
        let frames = decoder.push(&encoded).expect("the next frame parses");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].payload, b"ok");
    }

    #[test]
    fn the_decoder_rejects_a_push_larger_than_any_frame() {
        let mut decoder = FrameDecoder::new();

        let error = decoder
            .push(&vec![0u8; MAX_PAYLOAD + 7])
            .expect_err("too many bytes for one frame");

        assert!(matches!(error, FrameError::TooLarge { .. }));
        assert_eq!(decoder.pending(), 0);
    }

    #[test]
    fn the_decoder_waits_for_the_rest_of_a_frame() {
        let encoded = Frame::encode(b"hello").expect("encodes");
        let mut decoder = FrameDecoder::new();

        let first = decoder.push(&encoded[..4]).expect("no frame yet");
        assert!(first.is_empty(), "unexpected frames: {first:?}");
        assert_eq!(decoder.pending(), 4);

        let second = decoder.push(&encoded[4..]).expect("one frame");
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].payload, b"hello");
        assert_eq!(decoder.pending(), 0);
    }
}
