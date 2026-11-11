//! Values that do not fit the wire are refused when the protocol is built or
//! the command runs, never rounded into a different address.

use oxtuner_ini::{Definition, SettingsContext};
use oxtuner_protocol::{BehaviorKind, MsProtocol, ProtocolError};
use oxtuner_transport::{ReplayTransport, Trace};

fn protocol(definition: &Definition) -> MsProtocol {
    let behavior = BehaviorKind::select(definition).expect("a known family");

    MsProtocol::new(definition, behavior.as_dyn()).expect("the fixture is complete")
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
