//! Values that do not fit the wire are refused when the protocol is built or
//! the command runs, never rounded into a different address.

use oxtuner_ini::{CommandTemplate, Definition, SettingsContext};
use oxtuner_protocol::{
    BehaviorKind, BurnPlan, Crc, Crc32Variant, CrcReply, EcuBehavior, LinkMode, Match, MsProtocol,
    PageSwitch, ProtocolError, ReplyCode,
};
use oxtuner_transport::{ReplayTransport, Trace};

fn protocol(definition: &Definition) -> MsProtocol {
    let behavior = BehaviorKind::select(definition).expect("a known family");

    MsProtocol::new(definition, behavior.as_dyn()).expect("the fixture is complete")
}

/// A firmware that reports the page checksum as ASCII hex.
struct HexTextBehavior;

impl EcuBehavior for HexTextBehavior {
    fn name(&self) -> &'static str {
        "hex-text"
    }

    fn matches(&self, _definition: &Definition) -> Match {
        Match::Exact
    }

    fn handshake(&self, _definition: &Definition) -> Vec<CommandTemplate> {
        Vec::new()
    }

    fn crc(&self, _definition: &Definition, _page: u8) -> Option<Crc> {
        Some(Crc {
            algorithm: Crc32Variant::IsoHdlc,
            reply: CrcReply::HexText,
        })
    }

    fn switch_page(&self, _definition: &Definition, _page: u8) -> PageSwitch {
        PageSwitch::None
    }

    fn burn(&self, _definition: &Definition, _page: u8) -> BurnPlan {
        BurnPlan::NotBurnable {
            reason: "test firmware",
        }
    }

    fn reply_code(&self, _definition: &Definition) -> ReplyCode {
        ReplyCode::Leading
    }
}

#[test]
fn a_realtime_offset_above_the_wire_field_is_refused() {
    let definition = Definition::parse(
        "\
[MegaTune]
signature = \"speeduino 202501\"
queryCommand = \"Q\"

[Constants]
nPages = 1
pageSize = 8
page = 1
pageReadCommand = \"p%2i%2o%2c\"
outputChannelStartOffset = 65536
reqFuel = scalar, U08, 0, \"ms\", 0.1, 0, 0, 25.5, 1

[OutputChannels]
ochGetCommand = \"r%2o%2c\"
ochBlockSize = 8
rpm = scalar, U16, 0, \"rpm\", 1.0, 0.0
",
        &SettingsContext::new(),
    )
    .expect("parses");
    let behavior = BehaviorKind::select(&definition).expect("a known family");

    let error = MsProtocol::new(&definition, behavior.as_dyn()).expect_err("refused");

    assert!(matches!(error, ProtocolError::Unsupported(_)), "{error}");
    assert!(error.to_string().contains("65536"), "{error}");
}

#[test]
fn a_page_too_large_for_the_crc_count_is_refused() {
    let definition = Definition::parse(
        "\
[MegaTune]
signature = \"speeduino 202501\"
queryCommand = \"Q\"

[Constants]
nPages = 1
pageSize = 70000
page = 1
pageReadCommand = \"p%2i%2o%2c\"
pageCrcCommand = \"d%2i%2o%2c\"
reqFuel = scalar, U08, 0, \"ms\", 0.1, 0, 0, 25.5, 1
",
        &SettingsContext::new(),
    )
    .expect("parses");
    let mut protocol = protocol(&definition);
    let mut io = ReplayTransport::new(Trace::parse("").expect("an empty trace"));

    let error = protocol
        .read_page_crc(&mut io, 1)
        .expect_err("the page size does not fit the count field");

    assert!(matches!(error, ProtocolError::Unsupported(_)), "{error}");
    assert!(error.to_string().contains("70000"), "{error}");
}

#[test]
fn a_hex_text_crc_is_refused_over_the_legacy_link() {
    let definition = Definition::parse(
        "\
[MegaTune]
signature = \"speeduino 202501\"
queryCommand = \"Q\"

[Constants]
nPages = 1
pageSize = 8
page = 1
pageReadCommand = \"p%2i%2o%2c\"
pageCrcCommand = \"k%2i%2o%2c\"
reqFuel = scalar, U08, 0, \"ms\", 0.1, 0, 0, 25.5, 1
",
        &SettingsContext::new(),
    )
    .expect("parses");
    let behavior = HexTextBehavior;
    let mut protocol = MsProtocol::new(&definition, &behavior).expect("builds");
    let mut io = ReplayTransport::new(Trace::parse("").expect("an empty trace"));

    assert_eq!(protocol.link_mode(), LinkMode::Legacy);

    let error = protocol
        .read_page_crc(&mut io, 1)
        .expect_err("a text CRC cannot be read as four raw bytes");

    assert!(matches!(error, ProtocolError::Unsupported(_)), "{error}");
    assert!(error.to_string().contains("text"), "{error}");
}

#[test]
fn a_realtime_block_size_that_is_unusable_is_refused() {
    for size in [0, 70000] {
        let definition = Definition::parse(
            &format!(
                "\
[MegaTune]
signature = \"speeduino 202501\"
queryCommand = \"Q\"

[Constants]
nPages = 1
pageSize = 8
page = 1
pageReadCommand = \"p%2i%2o%2c\"
reqFuel = scalar, U08, 0, \"ms\", 0.1, 0, 0, 25.5, 1

[OutputChannels]
ochGetCommand = \"r%2o%2c\"
ochBlockSize = {size}
rpm = scalar, U16, 0, \"rpm\", 1.0, 0.0
"
            ),
            &SettingsContext::new(),
        )
        .expect("parses");
        let behavior = BehaviorKind::select(&definition).expect("a known family");

        let error = MsProtocol::new(&definition, behavior.as_dyn()).expect_err("refused");

        assert!(matches!(error, ProtocolError::Unsupported(_)), "{error}");
        assert!(error.to_string().contains(&size.to_string()), "{error}");
    }
}
