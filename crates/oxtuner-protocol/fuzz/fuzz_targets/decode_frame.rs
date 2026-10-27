#![no_main]

//! The framed protocol decoder must reject anything it cannot prove, and never
//! panic on it. Feed arbitrary bytes: a valid frame round-trips, everything else
//! is an error.

use libfuzzer_sys::fuzz_target;
use oxtuner_protocol::{Frame, FrameDecoder};

fuzz_target!(|data: &[u8]| {
    if let Ok(frame) = Frame::parse(data) {
        // A parsed frame must re-encode to the same bytes (the length and CRC
        // are the only framing).
        let encoded = Frame::encode(&frame.payload).expect("a parsed frame fits");
        assert_eq!(encoded, data);
    }

    let mut decoder = FrameDecoder::new();
    let _ = decoder.push(data);
});
