//! The framing decoder is fed untrusted bytes; it must never panic and must
//! never accept a frame it cannot prove.
//!
//! Property tests complement the fuzz targets: the fuzz job runs longer, these
//! run on every CI leg with the same invariant.

use oxtuner_ini::{CommandTemplate, Definition, SettingsContext};
use oxtuner_protocol::{
    BehaviorKind, CommandPattern, Frame, FrameDecoder, LinkMode, MsProtocol, Protocol,
    ProtocolError,
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
        let pattern = CommandPattern::new(&command, &|_| Some(0)).expect("a pattern");
        let _ = pattern.capture(&data);

        let command = CommandTemplate::parse("M%2i%2o%2c%v");
        let pattern = CommandPattern::new(&command, &|_| Some(0)).expect("a pattern");
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
    let behavior = BehaviorKind::select(&definition).expect("a family");
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

/// One framed reply, in the trace format the replay transport reads.
fn framed_reply_text(payload: &[u8]) -> String {
    let framed = Frame::encode(payload).expect("the payload fits a frame");
    let hex: Vec<String> = framed.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("RX 0 {}\n", hex.join(" "))
}

fn protocol() -> MsProtocol {
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
    let behavior = BehaviorKind::select(&definition).expect("a family");

    MsProtocol::new(&definition, behavior.as_dyn()).expect("complete")
}

fn envelope_protocol() -> MsProtocol {
    protocol().with_link_mode(LinkMode::Envelope)
}

#[test]
fn a_legacy_signature_reply_tolerates_leading_padding() {
    let mut protocol = protocol();
    let trace = Trace::parse("RX 0 0d 0a 73 70 65 65 64 75 69 6e 6f 20 32 30 32 35 30 31\n")
        .expect("a trace");
    let mut io = ReplayTransport::new(trace);
    io.open().expect("opens");

    let signature = protocol.signature(&mut io).expect("reads the signature");

    assert_eq!(signature, "speeduino 202501");
}

#[test]
fn a_signature_refusal_code_is_reported_as_a_return_code() {
    let mut protocol = envelope_protocol();
    let trace = Trace::parse(&framed_reply_text(&[1, b'x'])).expect("a trace");
    let mut io = ReplayTransport::new(trace);
    io.open().expect("opens");

    let error = protocol
        .signature(&mut io)
        .expect_err("the reply is a refusal");

    assert_eq!(
        error,
        ProtocolError::ReturnCode {
            code: 1,
            operation: "signature"
        }
    );
}

#[test]
fn a_signature_reply_stops_at_the_first_padding_byte() {
    let mut protocol = envelope_protocol();
    let trace = Trace::parse(&framed_reply_text(b"speeduino 202501\0leftover")).expect("a trace");
    let mut io = ReplayTransport::new(trace);
    io.open().expect("opens");

    let signature = protocol.signature(&mut io).expect("reads the signature");

    assert_eq!(signature, "speeduino 202501");
}

#[test]
fn a_signature_reply_tolerates_leading_padding() {
    let mut protocol = envelope_protocol();
    let trace = Trace::parse(&framed_reply_text(b"\r\n\0speeduino 202501")).expect("a trace");
    let mut io = ReplayTransport::new(trace);
    io.open().expect("opens");

    let signature = protocol.signature(&mut io).expect("reads the signature");

    assert_eq!(signature, "speeduino 202501");
}
