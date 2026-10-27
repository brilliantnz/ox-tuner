//! The framing decoder is fed untrusted bytes; it must never panic and must
//! never accept a frame it cannot prove.
//!
//! Property tests complement the fuzz targets: the fuzz job runs longer, these
//! run on every CI leg with the same invariant.

use oxtuner_ini::Command;
use oxtuner_protocol::{CommandPattern, Frame, FrameDecoder};
use proptest::prelude::*;

proptest! {
    #[test]
    fn arbitrary_bytes_never_make_the_frame_parser_panic(data: Vec<u8>) {
        if let Ok(frame) = Frame::parse(&data) {
            // A frame this build accepted must re-encode to exactly the bytes
            // it arrived as; otherwise a replay of it would differ.
            let encoded = Frame::encode(&frame.payload).expect("a parsed payload fits");
            prop_assert_eq!(encoded, data);
        }
    }

    #[test]
    fn arbitrary_bytes_never_make_the_stream_decoder_panic(data: Vec<u8>) {
        let mut decoder = FrameDecoder::new();
        let _ = decoder.push(&data);
    }

    #[test]
    fn a_concatenation_of_frames_decodes_to_the_same_frames(payloads in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..64), 1..8)) {
        let mut bytes = Vec::new();
        for payload in &payloads {
            if let Ok(encoded) = Frame::encode(payload) {
                bytes.extend(encoded);
            }
        }
        let mut decoder = FrameDecoder::new();
        let frames = decoder.push(&bytes).expect("our own frames parse");
        let decoded: Vec<Vec<u8>> = frames.into_iter().map(|frame| frame.payload).collect();
        prop_assert_eq!(decoded, payloads);
    }

    #[test]
    fn a_command_matcher_never_panics_on_wire_bytes(data: Vec<u8>) {
        let command = Command::parse("p%2i%2o%2c");
        let pattern = CommandPattern::new(&command, false, &|_| Some(0)).expect("a pattern");
        let _ = pattern.capture(&data);

        let command = Command::parse("M%2i%2o%2c%v");
        let pattern = CommandPattern::new(&command, false, &|_| Some(0)).expect("a pattern");
        let _ = pattern.capture(&data);
    }
}
