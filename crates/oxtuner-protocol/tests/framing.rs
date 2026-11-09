//! The framing decoder is fed untrusted bytes; it must never panic and must
//! never accept a frame it cannot prove.
//!
//! Property tests complement the fuzz targets: the fuzz job runs longer, these
//! run on every CI leg with the same invariant.

use oxtuner_ini::{CommandTemplate, Definition, SettingsContext};
use oxtuner_protocol::{
    Behavior, CommandPattern, Frame, FrameDecoder, LinkMode, MsProtocol, Protocol, ProtocolError,
};
use oxtuner_transport::{ReplayTransport, Trace, Transport};
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
        let command = CommandTemplate::parse("p%2i%2o%2c");
        let pattern = CommandPattern::new(&command, false, &|_| Some(0)).expect("a pattern");
        let _ = pattern.capture(&data);

        let command = CommandTemplate::parse("M%2i%2o%2c%v");
        let pattern = CommandPattern::new(&command, false, &|_| Some(0)).expect("a pattern");
        let _ = pattern.capture(&data);
    }
}

#[test]
fn an_oversized_reply_header_is_rejected_before_allocating() {
    let definition = Definition::parse(
        "\
[MegaTune]
signature = \"speeduino 202501\"
queryCommand = \"Q\"

[Constants]
endianness = little
nPages = 1
pageSize = 4
page = 1
pageReadCommand = \"p%2i%2o%2c\"
reqFuel = scalar, U08, 0, \"ms\", 0.1, 0, 0, 25.5, 1
",
        &SettingsContext::new(),
    )
    .expect("parses");
    let behavior = Behavior::select(&definition).expect("a family");
    let mut protocol = MsProtocol::new(&definition, behavior.as_dyn())
        .expect("complete")
        .with_link_mode(LinkMode::Envelope);
    // The reply starts with a length of 0xFFFF; no frame may be that large.
    let trace = Trace::parse("RX 0 ff ff\n").expect("a trace");
    let mut io = ReplayTransport::new(trace);
    io.open().expect("opens");

    let error = protocol.signature(&mut io).expect_err("oversized");

    assert!(matches!(error, ProtocolError::BadFrame(_)), "{error}");
}
