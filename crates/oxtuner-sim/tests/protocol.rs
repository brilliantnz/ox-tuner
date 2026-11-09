//! The M5 gate: the whole protocol stack against the virtual ECU.
//!
//! Nothing here touches hardware. The same `MsProtocol` the core uses talks to
//! `SimTransport`, which answers from the definition's own message set — so a
//! failure is a real protocol failure, not a mock drifting from production.

use oxtuner_ini::{Definition, SettingsContext};
use oxtuner_protocol::{Behavior, LinkMode, MsProtocol, Protocol};
use oxtuner_sim::{Fault, FaultRule, SimConfig, SimOp, SimTransport};
use oxtuner_transport::{Transport, TransportError};

/// A two-page definition in the Speeduino shape: a `p` read command, `M`
/// single-value writes, `b` burn, `d` CRC and a framed-protocol declaration.
fn speeduino() -> Definition {
    Definition::parse(
        "\
[MegaTune]
signature = \"speeduino 202501\"
queryCommand = \"Q\"

[PcVariables]
tsCanId = bits, U08, [0:0], \"CAN 0\", \"CAN 1\"

[Constants]
endianness = little
nPages = 2
pageSize = 12, 8
page = 1
pageIdentifier = \"\\$tsCanId\\x01\", \"\\$tsCanId\\x02\"
pageReadCommand = \"p%2i%2o%2c\", \"p%2i%2o%2c\"
pageValueWrite = \"M%2i%2o%2c%v\", \"M%2i%2o%2c%v\"
crc32CheckCommand = \"d%2i\", \"d%2i\"
burnCommand = \"b%2i\", \"\"
blockingFactor = 8
blockReadTimeout = 500
pageActivationDelay = 1
interWriteDelay = 1
tsWriteBlocks = on
messageEnvelopeFormat = msEnvelope_1.0
reqFuel = scalar, U08, 0, \"ms\", 0.1, 0, 0, 25.5, 1

[OutputChannels]
ochGetCommand = \"r\\$tsCanId\\x30%2o%2c\"
ochBlockSize = 8
rpm = scalar, U16, 0, \"rpm\", 1.0, 0.0
",
        &SettingsContext::new(),
    )
    .expect("the fixture parses")
}

/// A rusEFI-shaped definition: page identifiers are little-endian words, reads
/// are `R`, realtime is `O`, the signature is `S`, and the envelope is declared.
fn rusefi() -> Definition {
    Definition::parse(
        "\
[MegaTune]
signature = \"rusEFI v20240101\"
queryCommand = \"S\"

[Constants]
nPages = 2
pageSize = 12, 8
page = 1
pageIdentifier = \"\\x00\\x00\", \"\\x00\\x01\"
pageReadCommand = \"R%2i%2o%2c\", \"R%2i%2o%2c\"
pageValueWrite = \"C%2i%2o%2c%v\", \"C%2i%2o%2c%v\"
crc32CheckCommand = \"k%2i%2o%2c\", \"k%2i%2o%2c\"
burnCommand = \"B%2i\", \"\"
blockingFactor = 8
blockReadTimeout = 500
pageActivationDelay = 1
tsWriteBlocks = on
messageEnvelopeFormat = msEnvelope_1.0
reqFuel = scalar, U08, 0, \"ms\", 0.1, 0, 0, 25.5, 1

[OutputChannels]
ochGetCommand = \"O%2o%2c\"
ochBlockSize = 8
rpm = scalar, U16, 0, \"rpm\", 1.0, 0.0
",
        &SettingsContext::new(),
    )
    .expect("the fixture parses")
}

fn protocol(definition: &Definition, link: LinkMode) -> MsProtocol {
    let behavior = Behavior::select(definition).expect("a known family");
    MsProtocol::new(definition, behavior.as_dyn())
        .expect("the fixture is complete")
        .with_link_mode(link)
}

fn sim(definition: &Definition) -> SimTransport {
    SimTransport::new(definition, SimConfig::default()).expect("the fixture simulates")
}

#[test]
fn a_legacy_session_reads_writes_and_burns() {
    let definition = speeduino();
    let mut transport = sim(&definition);
    let mut protocol = protocol(&definition, LinkMode::Legacy);
    transport.open().expect("opens");

    let signature = protocol
        .signature(&mut transport)
        .expect("reads the signature");
    assert_eq!(signature, "speeduino 202501");

    // A page read is chunked at the blocking factor; the page size is not a
    // multiple of it, so the last chunk is short.
    let page = protocol
        .read_page_all(&mut transport, 1)
        .expect("reads page 1");
    assert_eq!(page.len(), 12);
    assert_eq!(page, transport.ram(1).expect("page 1"));

    let before = page[3];
    protocol
        .write_page(&mut transport, 1, 3, &[0xAB, 0xCD])
        .expect("writes two bytes");
    let after = protocol
        .read_page(&mut transport, 1, 0, 12)
        .expect("reads back");
    assert_eq!(after[3], 0xAB);
    assert_eq!(after[4], 0xCD);
    assert_ne!(after[3], before);

    protocol.burn(&mut transport, 1).expect("burns page 1");
    assert_eq!(
        transport.flash(1).expect("page 1"),
        transport.ram(1).expect("page 1")
    );

    // Page 2 has no burn command: the behavior says so, and the protocol
    // refuses before touching the wire.
    let behavior = Behavior::select(&definition).expect("a family");
    assert!(matches!(
        behavior.as_dyn().burn(&definition, 2),
        oxtuner_protocol::BurnPlan::NotBurnable { .. }
    ));
    assert!(protocol.burn(&mut transport, 2).is_err());
}

#[test]
fn a_framed_session_reads_writes_and_burns() {
    let definition = speeduino();
    let mut transport = sim(&definition);
    let mut protocol = protocol(&definition, LinkMode::Envelope);
    transport.open().expect("opens");

    assert_eq!(
        protocol.signature(&mut transport).expect("signature"),
        "speeduino 202501"
    );
    let page = protocol
        .read_page_all(&mut transport, 1)
        .expect("reads page 1");
    assert_eq!(page, transport.ram(1).expect("page 1"));

    protocol
        .write_page(&mut transport, 1, 0, &[1, 2, 3])
        .expect("writes");
    assert_eq!(&transport.ram(1).expect("page 1")[..3], &[1, 2, 3]);
    protocol.burn(&mut transport, 1).expect("burns");
    assert_eq!(transport.flash(1), transport.ram(1));
}

#[test]
fn a_rusefi_shaped_definition_runs_without_special_cases() {
    let definition = rusefi();
    let mut transport = sim(&definition);
    let mut protocol = protocol(&definition, LinkMode::Envelope);
    transport.open().expect("opens");

    assert_eq!(
        protocol.signature(&mut transport).expect("signature"),
        "rusEFI v20240101"
    );

    // Page 2's wire identifier is `[0x00, 0x01]`, not `[0, 2]`: the definition
    // decides, and the simulator matched the same bytes.
    let page = protocol
        .read_page_all(&mut transport, 2)
        .expect("reads page 2");
    assert_eq!(page.len(), 8);
    assert_eq!(page, transport.ram(2).expect("page 2"));

    protocol
        .write_page(&mut transport, 1, 0, &[9, 8])
        .expect("writes");
    assert_eq!(&transport.ram(1).expect("page 1")[..2], &[9, 8]);

    // The CRC command takes a range: rusEFI's `k%2i%2o%2c`.
    let crc = protocol
        .read_page_crc(&mut transport, 1)
        .expect("CRC read")
        .expect("the definition declares one");
    assert_eq!(
        crc,
        oxtuner_protocol::crc32(transport.ram(1).expect("page 1"))
    );
}

#[test]
fn a_dropped_reply_is_a_timeout() {
    let definition = speeduino();
    let mut transport = sim(&definition);
    transport.arm(SimOp::ReadPage, Fault::DropReply);
    let mut protocol = protocol(&definition, LinkMode::Legacy);
    transport.open().expect("opens");

    let error = protocol
        .read_page(&mut transport, 1, 0, 4)
        .expect_err("no reply");

    assert!(error.is_timeout(), "{error}");
}

#[test]
fn a_truncated_reply_is_a_timeout_not_a_short_page() {
    let definition = speeduino();
    let mut transport = sim(&definition);
    transport.arm(SimOp::ReadPage, Fault::Truncate { keep: 2 });
    let mut protocol = protocol(&definition, LinkMode::Legacy);
    transport.open().expect("opens");

    let error = protocol
        .read_page(&mut transport, 1, 0, 4)
        .expect_err("only two bytes");

    assert!(error.is_timeout(), "{error}");
}

#[test]
fn a_disconnect_is_a_link_loss() {
    let definition = speeduino();
    let mut transport = sim(&definition);
    transport.arm(SimOp::ReadPage, Fault::Disconnect);
    let mut protocol = protocol(&definition, LinkMode::Legacy);
    transport.open().expect("opens");

    let error = protocol
        .read_page(&mut transport, 1, 0, 4)
        .expect_err("the link died");

    assert!(error.is_link_loss(), "{error}");
    assert!(!transport.is_connected());
}

#[test]
fn a_dropped_reply_does_not_desynchronise_the_next_conversation_once_flushed() {
    let definition = speeduino();
    let mut transport = sim(&definition);
    transport.arm(SimOp::Signature, Fault::DropReply);
    let mut protocol = protocol(&definition, LinkMode::Legacy);
    transport.open().expect("opens");

    assert!(protocol.signature(&mut transport).is_err());

    // The ECU is still there; a fresh query (after the caller's own retry
    // policy allows one) answers. The protocol itself never retried.
    let signature = protocol.signature(&mut transport).expect("second query");
    assert_eq!(signature, "speeduino 202501");
}

#[test]
fn a_refused_burn_is_reported_in_framed_mode() {
    let definition = speeduino();
    let mut transport = sim(&definition);
    transport.arm(SimOp::Burn, Fault::RefuseBurn);
    let mut protocol = protocol(&definition, LinkMode::Envelope);
    transport.open().expect("opens");

    let error = protocol.burn(&mut transport, 1).expect_err("refused");

    assert!(error.to_string().contains("burn"), "{error}");
}

#[test]
fn the_same_sequence_produces_the_same_transcript() {
    fn run() -> String {
        let definition = speeduino();
        let mut transport = sim(&definition);
        let mut protocol = protocol(&definition, LinkMode::Legacy);
        transport.open().expect("opens");
        protocol.signature(&mut transport).expect("signature");
        protocol.read_page_all(&mut transport, 1).expect("page 1");
        protocol.read_realtime(&mut transport).expect("realtime");
        protocol.read_page_all(&mut transport, 2).expect("page 2");
        transport.transcript().to_text()
    }

    assert_eq!(run(), run());
}

#[test]
fn every_page_and_realtime_block_round_trips_at_the_declared_size() {
    let definition = speeduino();
    let mut transport = sim(&definition);
    let mut protocol = protocol(&definition, LinkMode::Envelope);
    transport.open().expect("opens");

    for page in &definition.pages {
        let bytes = protocol
            .read_page_all(&mut transport, page.number)
            .expect("reads");
        assert_eq!(bytes.len(), usize::try_from(page.size).unwrap_or(0));
    }

    let block = protocol.read_realtime(&mut transport).expect("realtime");
    assert_eq!(
        block.len(),
        usize::try_from(definition.comms.och_block_size.expect("declared")).unwrap_or(0)
    );
}

#[test]
fn a_scenario_arms_a_fault_and_preloads_a_page() {
    let definition = speeduino();
    let scenario = oxtuner_sim::Scenario::parse(
        "seed = 3\npage = 1 : 01 02 03 04 05 06 07 08 09 0a 0b 0c\nfault = signature:drop\n",
    )
    .expect("a scenario");
    let mut transport =
        SimTransport::with_scenario(&definition, &scenario).expect("simulates from the scenario");
    let mut protocol = protocol(&definition, LinkMode::Legacy);
    transport.open().expect("opens");

    assert!(protocol.signature(&mut transport).is_err());
    assert_eq!(
        transport.ram(1).expect("page 1"),
        &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]
    );
}

#[test]
fn a_write_that_runs_past_the_page_is_refused_before_the_wire() {
    let definition = speeduino();
    let mut transport = sim(&definition);
    let mut protocol = protocol(&definition, LinkMode::Legacy);
    transport.open().expect("opens");

    let error = protocol
        .write_page(&mut transport, 1, 10, &[1, 2, 3])
        .expect_err("10 + 3 > 12");

    assert!(error.to_string().contains("page 1"), "{error}");
    assert!(transport.transcript().is_empty(), "nothing was sent");
}

#[test]
fn a_fault_is_consumed_once_so_the_session_can_recover() {
    let definition = speeduino();
    let mut transport = sim(&definition);
    transport.add_rule(FaultRule::new(SimOp::ReadPage, Fault::DropReply, 1));
    let mut protocol = protocol(&definition, LinkMode::Legacy);
    transport.open().expect("opens");

    assert!(protocol.read_page(&mut transport, 1, 0, 4).is_err());
    assert!(protocol.read_page(&mut transport, 1, 0, 4).is_ok());
}

#[test]
fn a_read_does_not_change_the_page() {
    let definition = speeduino();
    let mut transport = sim(&definition);
    let mut protocol = protocol(&definition, LinkMode::Legacy);
    transport.open().expect("opens");
    let before = transport.ram(1).expect("page 1").to_vec();

    protocol.read_page_all(&mut transport, 1).expect("reads");

    assert_eq!(transport.ram(1), Some(before.as_slice()));
}

#[test]
fn closing_the_simulator_stops_the_conversation() {
    let definition = speeduino();
    let mut transport = sim(&definition);
    let mut protocol = protocol(&definition, LinkMode::Legacy);
    transport.open().expect("opens");
    transport.close();

    let error = protocol.signature(&mut transport).expect_err("closed");

    assert!(matches!(
        error,
        oxtuner_protocol::ProtocolError::Transport(TransportError::NotOpen)
    ));
}

#[test]
fn the_simulator_sustains_100_hz_realtime() {
    let definition = speeduino();
    let mut transport = sim(&definition);
    let mut protocol = protocol(&definition, LinkMode::Legacy);
    transport.open().expect("opens");

    // The spec's performance smoke: 100 blocks on demand, with no backlog and
    // no sleeping in the transport. On the sim this is CPU work only.
    let started = std::time::Instant::now();
    for _ in 0..100 {
        let block = protocol.read_realtime(&mut transport).expect("block");
        assert_eq!(block.len(), 8);
    }
    let elapsed = started.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "100 realtime blocks took {elapsed:?}"
    );
}

#[test]
fn a_bad_frame_crc_is_rejected() {
    let definition = speeduino();
    let mut transport = sim(&definition);
    transport.arm(SimOp::ReadPage, Fault::BadCrc);
    let mut protocol = protocol(&definition, LinkMode::Envelope);
    transport.open().expect("opens");

    let error = protocol
        .read_page(&mut transport, 1, 0, 4)
        .expect_err("the CRC no longer matches");

    assert!(
        matches!(error, oxtuner_protocol::ProtocolError::BadFrame(_)),
        "{error}"
    );
}

#[test]
fn a_partial_write_leaves_the_rest_of_the_page_untouched() {
    let definition = speeduino();
    let mut transport = sim(&definition);
    let before = transport.ram(1).expect("page 1").to_vec();
    transport.arm(SimOp::WritePage, Fault::PartialWrite { bytes: 1 });
    let mut protocol = protocol(&definition, LinkMode::Legacy);
    transport.open().expect("opens");

    protocol
        .write_page(&mut transport, 1, 0, &[0xAA, 0xBB])
        .expect("the command is sent");

    let after = transport.ram(1).expect("page 1");
    assert_eq!(after[0], 0xAA, "the first byte landed");
    assert_eq!(after[1], before[1], "the second byte did not");
}
