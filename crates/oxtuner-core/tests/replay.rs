//! Replay of a real NO2C session, without the ECU.
//!
//! The fixture is a read-only capture of the bench board (firmware
//! `speeduino 202501`, definition of the same version): the signature query,
//! every declared page and one realtime block, recorded through the production
//! transport stack. Replaying it here proves the wire format still matches what
//! the hardware actually sent (ADR-0012), and that the decoder still produces
//! the values `TunerStudio` would show.
//!
//! The bench capture is in `tests/fixtures/no2c-202501-envelope.trace`.

use std::path::PathBuf;

use oxtuner_core::RealtimeDecoder;
use oxtuner_ini::SettingsContext;
use oxtuner_protocol::{BehaviorKind, Frame, LinkMode, MsProtocol, Protocol};
use oxtuner_transport::{Direction, ReplayTransport, Trace, Transport};

/// The parts of the real `speeduino 202501` definition the replay needs.
///
/// The project does not bundle the firmware's INI (it is user-supplied), so the
/// fixture's message set is reproduced: the same signatures, page table,
/// commands, blocking factor and the realtime offsets of the channels this test
/// checks. The byte stream still comes from the hardware capture.
fn definition() -> oxtuner_ini::Definition {
    let text = "\
[MegaTune]
signature = \"speeduino 202501\"
queryCommand = \"Q\"

[PcVariables]
tsCanId = bits, U08, [0:0], \"CAN 0\", \"CAN 1\"

[Constants]
endianness = little
nPages = 15
pageSize = 128, 288, 288, 128, 288, 128, 240, 384, 192, 192, 288, 192, 128, 288, 256
page = 1
pageIdentifier = \"\\$tsCanId\\x01\", \"\\$tsCanId\\x02\", \"\\$tsCanId\\x03\", \"\\$tsCanId\\x04\", \"\\$tsCanId\\x05\", \"\\$tsCanId\\x06\", \"\\$tsCanId\\x07\", \"\\$tsCanId\\x08\", \"\\$tsCanId\\x09\", \"\\$tsCanId\\x0A\", \"\\$tsCanId\\x0B\", \"\\$tsCanId\\x0C\", \"\\$tsCanId\\x0D\", \"\\$tsCanId\\x0E\", \"\\$tsCanId\\x0F\"
pageReadCommand = \"p%2i%2o%2c\", \"p%2i%2o%2c\", \"p%2i%2o%2c\", \"p%2i%2o%2c\", \"p%2i%2o%2c\", \"p%2i%2o%2c\", \"p%2i%2o%2c\", \"p%2i%2o%2c\", \"p%2i%2o%2c\", \"p%2i%2o%2c\", \"p%2i%2o%2c\", \"p%2i%2o%2c\", \"p%2i%2o%2c\", \"p%2i%2o%2c\", \"p%2i%2o%2c\"
blockingFactor = 251
blockReadTimeout = 2000
pageActivationDelay = 10
tsWriteBlocks = on
messageEnvelopeFormat = msEnvelope_1.0
reqFuel = scalar, U08, 0, \"ms\", 0.1, 0, 0, 25.5, 1

[OutputChannels]
ochGetCommand = \"r\\$tsCanId\\x30%2o%2c\"
ochBlockSize = 130
rpm = scalar, U16, 14, \"rpm\", 1.0, 0.0
tps = scalar, U08, 25, \"%\", 0.500, 0.0
";
    oxtuner_ini::parse_str(text, &SettingsContext::new()).expect("the fixture definition parses")
}

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/no2c-202501-envelope.trace")
}

#[test]
fn the_recorded_no2c_session_replays_without_hardware() {
    let trace = Trace::load(&fixture()).expect("the fixture parses");
    assert!(trace.len() > 30, "the fixture records a full session");

    // No-write proof over the recording itself: every request is a read.
    for frame in trace
        .frames()
        .iter()
        .filter(|frame| frame.direction == Direction::Tx)
    {
        let command = match Frame::parse(&frame.bytes) {
            Ok(frame) => *frame.payload.first().expect("a command byte"),
            Err(_) => *frame.bytes.first().expect("a command byte"),
        };
        assert!(
            matches!(command, b'Q' | b'p' | b'r'),
            "the fixture contains a non-read command: 0x{command:02x}"
        );
    }

    let definition = definition();
    let behavior = BehaviorKind::select(&definition).expect("a known family");
    let mut protocol = MsProtocol::new(&definition, behavior.as_dyn())
        .expect("the definition is complete")
        .with_link_mode(LinkMode::Envelope);
    let mut replay = ReplayTransport::new(trace.clone());
    replay.open().expect("the replay opens");

    assert_eq!(
        protocol
            .signature(&mut replay)
            .expect("the recorded signature"),
        "speeduino 202501"
    );

    let mut total = 0usize;
    for page in &definition.pages {
        let bytes = protocol
            .read_page_all(&mut replay, page.number)
            .expect("the recorded page read");
        assert_eq!(bytes.len(), usize::try_from(page.size).unwrap_or(0));
        total += bytes.len();
    }
    assert_eq!(total, 3_408);

    let block = protocol
        .read_realtime(&mut replay)
        .expect("the recorded realtime block");
    assert_eq!(block.len(), 130);

    let decoder = RealtimeDecoder::new(&definition);
    let frame = decoder.decode(&block, 0);
    assert_eq!(frame.values.len(), 2, "the fixture declares two channels");
    let rpm = frame.value("rpm").expect("the definition declares rpm");
    // The bench board has no engine turning; a floating trigger input reported
    // 250 rpm at capture time. The exact number proves the offsets and byte
    // order survived the replay.
    assert_eq!(rpm.value, 250.0);

    // Replaying twice must produce the same bytes.
    let mut second = ReplayTransport::new(trace);
    second.open().expect("opens");
    let mut protocol = MsProtocol::new(&definition, behavior.as_dyn())
        .expect("complete")
        .with_link_mode(LinkMode::Envelope);
    protocol.signature(&mut second).expect("signature");
    for page in &definition.pages {
        protocol
            .read_page_all(&mut second, page.number)
            .expect("page");
    }
    assert_eq!(
        protocol.read_realtime(&mut second).expect("realtime"),
        block
    );
}

#[test]
fn one_hundred_replays_are_identical() {
    // The determinism acceptance from the simulator spec: the same recording
    // produces the same answers every time, with no hardware and no timing.
    let mut definition = definition();
    // The bench definition asks for a 10 ms page-activation delay. It changes
    // timing, not bytes, so the determinism loop turns it off: 100 full replays
    // then finish in well under a second instead of spending 15 s asleep.
    definition.comms.page_activation_delay_ms = None;
    let trace = Trace::load(&fixture()).expect("the fixture parses");
    let behavior = BehaviorKind::select(&definition).expect("a known family");

    let expected = run(&trace, &definition, behavior);
    for _ in 0..100 {
        assert_eq!(run(&trace, &definition, behavior), expected);
    }
}

/// Replays the whole capture and returns the signature, page bytes and block.
fn run(
    trace: &Trace,
    definition: &oxtuner_ini::Definition,
    behavior: BehaviorKind,
) -> (String, Vec<u8>, Vec<u8>) {
    let mut replay = ReplayTransport::new(trace.clone());
    replay.open().expect("opens");
    let mut protocol = MsProtocol::new(definition, behavior.as_dyn())
        .expect("complete")
        .with_link_mode(LinkMode::Envelope);
    let signature = protocol.signature(&mut replay).expect("signature");
    // The replay answers in recorded order, so the sequence must match the
    // capture: every page, then the realtime block.
    let mut pages = Vec::new();
    for page in &definition.pages {
        pages.extend(
            protocol
                .read_page_all(&mut replay, page.number)
                .expect("page"),
        );
    }
    let block = protocol.read_realtime(&mut replay).expect("realtime");
    (signature, pages, block)
}
