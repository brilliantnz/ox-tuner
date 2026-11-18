//! A framed write is acknowledged differently by each family, and the wrong
//! shape must be a refusal rather than a silent success.
//!
//! Speeduino answers with exactly one return code byte, so only `0x00` means
//! the write landed; rusEFI and FOME answer with an empty payload. These tests
//! drive the whole `write_page()` conversation, because that is the path a tune
//! write takes, and a permissive helper would still look correct on its own.

use oxtuner_ini::{Definition, SettingsContext};
use oxtuner_protocol::{BehaviorKind, Frame, MsProtocol, Protocol, ProtocolError};
use oxtuner_transport::{ReplayTransport, Trace, Transport};

/// A Speeduino-shaped definition: the `p` read, `M` value write and the
/// envelope the real INI declares.
fn speeduino() -> Definition {
    Definition::parse(
        "\
[MegaTune]
signature = \"speeduino 202501\"
queryCommand = \"Q\"

[Constants]
endianness = little
nPages = 1
pageSize = 8
page = 1
pageIdentifier = \"\\x00\\x01\"
pageReadCommand = \"p%2i%2o%2c\"
pageValueWrite = \"M%2i%2o%2c%v\"
blockingFactor = 8
messageEnvelopeFormat = msEnvelope_1.0
reqFuel = scalar, U08, 0, \"ms\", 0.1, 0, 0, 25.5, 1
",
        &SettingsContext::new(),
    )
    .expect("the fixture parses")
}

/// A rusEFI-shaped definition: the `R` read, `C` value write and the envelope
/// the real INI declares.
fn rusefi() -> Definition {
    Definition::parse(
        "\
[MegaTune]
signature = \"rusEFI v20240101\"
queryCommand = \"S\"

[Constants]
nPages = 1
pageSize = 8
page = 1
pageIdentifier = \"\\x00\\x00\"
pageReadCommand = \"R%2i%2o%2c\"
pageValueWrite = \"C%2i%2o%2c%v\"
blockingFactor = 8
messageEnvelopeFormat = msEnvelope_1.0
reqFuel = scalar, U08, 0, \"ms\", 0.1, 0, 0, 25.5, 1
",
        &SettingsContext::new(),
    )
    .expect("the fixture parses")
}

/// The protocol the definition selects on its own, envelope included.
fn protocol(definition: &Definition) -> MsProtocol {
    let behavior = BehaviorKind::select(definition).expect("a known family");

    MsProtocol::new(definition, behavior.as_dyn()).expect("the fixture is complete")
}

/// One framed reply, in the trace format the replay transport reads.
fn framed_reply(payload: &[u8]) -> Trace {
    let framed = Frame::encode(payload).expect("the payload fits a frame");
    let hex: Vec<String> = framed.iter().map(|byte| format!("{byte:02x}")).collect();

    Trace::parse(&format!("RX 0 {}\n", hex.join(" "))).expect("a trace")
}

/// Writes two bytes to page 1 with the family's protocol and returns the
/// outcome.
fn write(definition: &Definition, reply: &[u8]) -> Result<(), ProtocolError> {
    let mut protocol = protocol(definition);
    let mut io = ReplayTransport::new(framed_reply(reply));
    io.open().expect("opens");

    protocol.write_page(&mut io, 1, 0, &[0xAB, 0xCD])
}

#[test]
fn a_speeduino_write_is_accepted_by_a_zero_return_code() {
    write(&speeduino(), &[0]).expect("the return code says the write landed");
}

#[test]
fn a_speeduino_write_without_a_return_code_is_refused() {
    let error = write(&speeduino(), &[]).expect_err("the acknowledgement is missing");

    match error {
        ProtocolError::UnexpectedReply(message) => {
            assert!(message.contains("page write"), "{message}");
            assert!(message.contains("return code"), "{message}");
        }
        other => panic!("expected a shape error, got {other}"),
    }
}

#[test]
fn a_speeduino_write_refusal_is_a_return_code() {
    let error = write(&speeduino(), &[0x84]).expect_err("the ECU refused the write");

    assert_eq!(
        error,
        ProtocolError::ReturnCode {
            code: 0x84,
            operation: "page write"
        }
    );
}

#[test]
fn a_speeduino_write_with_trailing_bytes_is_refused() {
    let error = write(&speeduino(), &[0, 0xAB]).expect_err("an acknowledgement is one byte");

    match error {
        ProtocolError::UnexpectedReply(message) => {
            assert!(message.contains("page write"), "{message}");
            assert!(message.contains("2 byte(s)"), "{message}");
        }
        other => panic!("expected a shape error, got {other}"),
    }
}

#[test]
fn a_rusefi_write_is_accepted_by_an_empty_payload() {
    write(&rusefi(), &[]).expect("an empty acknowledgement is the family's shape");
}

#[test]
fn a_rusefi_write_refusal_is_a_return_code() {
    let error = write(&rusefi(), &[0x84]).expect_err("the ECU refused the write");

    assert_eq!(
        error,
        ProtocolError::ReturnCode {
            code: 0x84,
            operation: "page write"
        }
    );
}

#[test]
fn a_rusefi_write_that_leads_with_a_zero_code_is_refused() {
    let error = write(&rusefi(), &[0]).expect_err("this family never leads with a code");

    match error {
        ProtocolError::UnexpectedReply(message) => {
            assert!(message.contains("page write"), "{message}");
            assert!(message.contains("empty payload"), "{message}");
        }
        other => panic!("expected a shape error, got {other}"),
    }
}

#[test]
fn a_rusefi_write_with_two_bytes_is_refused() {
    let error = write(&rusefi(), &[0x00, 0x01]).expect_err("this family acknowledges with nothing");

    match error {
        ProtocolError::UnexpectedReply(message) => {
            assert!(message.contains("page write"), "{message}");
            assert!(message.contains("2 byte(s)"), "{message}");
        }
        other => panic!("expected a shape error, got {other}"),
    }
}
