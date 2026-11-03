//! Simulator benchmarks: building the virtual ECU, and one full conversation
//! against it through the production protocol.
//!
//! Run locally with:
//!
//! ```text
//! cargo bench -p oxtuner-sim
//! ```
//!
//! The interesting number is `sim_realtime_read`: the request is encoded by
//! `MsProtocol`, answered by the simulator from the definition's own offsets
//! and ranges, and decoded back. That is the whole fake-ECU path per realtime
//! frame, which the tests run thousands of times.
//!
//! The definition is synthetic and no hardware is touched.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use oxtuner_ini::{Definition, SettingsContext, parse_str};
use oxtuner_protocol::{Behavior, LinkMode, MsProtocol, Protocol};
use oxtuner_sim::{SimConfig, SimTransport};
use oxtuner_transport::Transport;

/// Two pages, the `p`/`M`/`b` command set and an eight-byte realtime block.
const INI: &str = "\
[MegaTune]
signature = \"speeduino 202501\"
queryCommand = \"Q\"

[Constants]
nPages = 2
pageSize = 12, 8
page = 1
pageIdentifier = \"\\x00\\x01\", \"\\x00\\x02\"
pageReadCommand = \"p%2i%2o%2c\", \"p%2i%2o%2c\"
pageValueWrite = \"M%2i%2o%2c%v\", \"M%2i%2o%2c%v\"
burnCommand = \"b%2i\", \"\"
blockingFactor = 8
blockReadTimeout = 500
tsWriteBlocks = on
reqFuel = scalar, U08, 0, \"ms\", 0.1, 0, 0, 25.5, 1
page = 2
baro = scalar, U08, 0, \"kpa\", 1.0, 0.0

[OutputChannels]
ochGetCommand = \"r%2o%2c\"
ochBlockSize = 8
rpm = scalar, U16, 0, \"rpm\", 1.0, 0.0
map = scalar, U16, 2, \"kpa\", 1.0, 0.0
";

fn definition() -> Definition {
    parse_str(INI, &SettingsContext::new()).expect("parses")
}

fn bench_build(criterion: &mut Criterion) {
    let definition = definition();

    criterion.bench_function("sim_build_two_pages", |bencher| {
        bencher.iter(|| {
            black_box(
                SimTransport::new(black_box(&definition), SimConfig::default()).expect("builds"),
            )
        });
    });
}

fn bench_realtime_read(criterion: &mut Criterion) {
    let definition = definition();
    let behavior = Behavior::select(&definition).expect("a known family");
    let mut protocol = MsProtocol::new(&definition, behavior.as_dyn())
        .expect("the definition is complete")
        .with_link_mode(LinkMode::Legacy);
    let mut transport = SimTransport::new(&definition, SimConfig::default()).expect("builds");

    transport.open().expect("opens");
    protocol
        .read_realtime(&mut transport)
        .expect("the fixture answers");

    criterion.bench_function("sim_realtime_read", |bencher| {
        bencher.iter(|| {
            black_box(
                protocol
                    .read_realtime(black_box(&mut transport))
                    .expect("reads"),
            )
        });
    });
}

fn bench_page_read(criterion: &mut Criterion) {
    let definition = definition();
    let behavior = Behavior::select(&definition).expect("a known family");
    let mut protocol = MsProtocol::new(&definition, behavior.as_dyn())
        .expect("the definition is complete")
        .with_link_mode(LinkMode::Legacy);
    let mut transport = SimTransport::new(&definition, SimConfig::default()).expect("builds");

    transport.open().expect("opens");
    protocol
        .read_page_all(&mut transport, 1)
        .expect("the fixture answers");

    criterion.bench_function("sim_page_read_12_bytes", |bencher| {
        bencher.iter(|| {
            black_box(
                protocol
                    .read_page_all(black_box(&mut transport), 1)
                    .expect("reads"),
            )
        });
    });
}

criterion_group!(benches, bench_build, bench_realtime_read, bench_page_read);
criterion_main!(benches);
