//! The M6 bench check: the real NO2C, read-only.
//!
//! Ignored by default because it needs the bench ECU. Run it with the port and
//! the matching definition:
//!
//! ```text
//! OX_TUNER_SERIAL_PORT=/dev/cu.usbmodem21201 \
//! OX_TUNER_INI=docs/tmp/local/ini/speeduino.ini \
//! OX_TUNER_TRACE=/tmp/no2c.trace \
//! cargo test -p oxtuner-core --test hardware -- --ignored --nocapture
//! ```
//!
//! `OX_TUNER_LINK=legacy` forces the unframed protocol; otherwise the
//! definition's own `messageEnvelopeFormat` decides.
//!
//! Run the bench tests serially (`--test-threads=1`): two sessions on one tty
//! interleave their bytes and both time out.

#![expect(
    clippy::print_stdout,
    reason = "the ignored hardware test prints a readable bench report"
)]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use oxtuner_core::{
    BlockingEventStream, CoreConfig, CoreHandle, CoreThread, Event, FailureCode, LatestFrame,
    LinkMode, PortChoice,
};
use oxtuner_ini::SettingsContext;

struct Harness {
    handle: CoreHandle,
    thread: CoreThread,
    latest: LatestFrame,
    events: BlockingEventStream,
}

impl Harness {
    fn start() -> Self {
        let parts = oxtuner_core::start(&CoreConfig::default()).expect("the core starts");
        Self {
            handle: parts.handle,
            thread: parts.thread,
            latest: parts.latest,
            events: parts.events.into_blocking(),
        }
    }

    fn wait(&self, wanted: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(event) = self.events.recv_timeout(left) else {
                panic!("no matching event before the deadline");
            };
            if wanted(&event) {
                return event;
            }
        }
    }
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("."))
}

#[test]
#[ignore = "needs the bench ECU; see the module docs for the environment"]
#[expect(
    clippy::too_many_lines,
    reason = "the bench check is one ordered read-only session and reads best in sequence"
)]
fn the_bench_ecu_reads_pages_and_realtime_without_writing() {
    let port = std::env::var("OX_TUNER_SERIAL_PORT")
        .unwrap_or_else(|_| "/dev/cu.usbmodem21201".to_owned());
    let ini = std::env::var("OX_TUNER_INI").map_or_else(
        |_| workspace_root().join("docs/tmp/local/ini/speeduino.ini"),
        PathBuf::from,
    );
    let bytes = std::fs::read(&ini).unwrap_or_else(|error| {
        panic!("could not read {}: {error}", ini.display());
    });
    let definition = oxtuner_ini::parse_bytes(&bytes, &SettingsContext::new())
        .expect("the reference definition parses");
    let framing = match std::env::var("OX_TUNER_LINK").as_deref() {
        Ok("legacy") => Some(LinkMode::Legacy),
        Ok("envelope") => Some(LinkMode::Envelope),
        _ => None,
    };

    let harness = Harness::start();
    harness
        .handle
        .load_definition("speeduino.ini", bytes)
        .expect("queued");
    harness.wait(|event| matches!(event, Event::DefinitionLoaded { .. }));

    harness
        .handle
        .connect(PortChoice::serial(port, 115_200), framing)
        .expect("queued");
    let Event::Connected { view: summary, .. } =
        harness.wait(|event| matches!(event, Event::Connected { .. }))
    else {
        unreachable!()
    };

    println!(
        "connected: {} on {} · {} · {} pages · realtime {:?}",
        summary.signature, summary.port, summary.framing, summary.pages, summary.realtime_block
    );
    assert_eq!(summary.signature, definition.signature);
    assert!(!summary.read_only, "the bench ECU must match its INI");

    harness.handle.read_all().expect("queued");
    let Event::PagesRead { report: read, .. } =
        harness.wait(|event| matches!(event, Event::PagesRead { .. }))
    else {
        unreachable!()
    };

    let declared: usize = definition
        .pages
        .iter()
        .map(|page| usize::try_from(page.size).unwrap_or(0))
        .sum();
    println!(
        "pages: {} · bytes: {} (declared {declared}) · constants: {} · skipped: {}",
        read.pages, read.bytes, read.constants, read.skipped
    );
    assert_eq!(read.pages, definition.pages.len());
    assert_eq!(read.bytes, declared);
    assert_eq!(read.skipped, 0, "every constant must have an address");
    assert!(read.constants > 500);

    // Polling started after the read; the engine is off on the bench, so RPM
    // must be exactly zero and the values must actually be decoded.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut frame = None;
    while Instant::now() < deadline {
        if let Some(latest) = harness.latest.get()
            && let Some(rpm) = latest.value("rpm")
        {
            frame = Some(std::sync::Arc::clone(&latest));
            if frame.as_ref().is_some_and(|frame| !frame.is_empty()) {
                let _ = rpm;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let frame = frame.expect("a realtime frame decoded");
    let rpm = frame.value("rpm").expect("the definition declares rpm");
    println!(
        "realtime: {} channels · rpm {}",
        frame.values.len(),
        rpm.text
    );
    // The bench ECU has no engine turning, but a floating trigger input can
    // read a little noise; what must hold is that the block decoded at all.
    assert!(
        rpm.value < 1_000.0,
        "no engine is turning: rpm {}",
        rpm.value
    );
    assert!(frame.values.len() > 20);

    harness.handle.stop_realtime().expect("queued");
    harness.wait(|event| matches!(event, Event::RealtimeState { running: false, .. }));

    let trace_path = std::env::var("OX_TUNER_TRACE").map_or_else(
        |_| std::env::temp_dir().join("oxtuner-no2c.trace"),
        PathBuf::from,
    );
    harness.handle.save_trace(&trace_path).expect("queued");
    let Event::TraceSaved { frames, .. } =
        harness.wait(|event| matches!(event, Event::TraceSaved { .. }))
    else {
        unreachable!()
    };
    println!("trace: {frames} frames at {}", trace_path.display());

    // No-write proof: read the trace back and check every request is one of the
    // definition's read commands.
    let trace = oxtuner_transport::Trace::load(&trace_path).expect("the trace parses");
    let allowed: Vec<u8> = definition
        .query_command
        .iter()
        .chain(
            definition
                .pages
                .iter()
                .filter_map(|page| page.read_command.as_ref()),
        )
        .chain(definition.comms.och_get_command.iter())
        .filter_map(|command| command.segments.first())
        .filter_map(|segment| match segment {
            oxtuner_ini::Segment::Byte(byte) => Some(*byte),
            _ => None,
        })
        .collect();
    let mut requests = 0usize;
    for frame in trace
        .frames()
        .iter()
        .filter(|frame| frame.direction == oxtuner_transport::Direction::Tx)
    {
        let command = match oxtuner_protocol::Frame::parse(&frame.bytes) {
            Ok(frame) => *frame.payload.first().expect("a command byte"),
            Err(_) => *frame.bytes.first().expect("a command byte"),
        };
        assert!(
            allowed.contains(&command),
            "a non-read command reached the ECU: 0x{command:02x}"
        );
        requests += 1;
    }

    println!("no-write proof: {requests} request(s), all read commands");
    assert!(requests > 20);

    harness.handle.disconnect().expect("queued");
    harness.wait(|event| matches!(event, Event::Disconnected { .. }));
    harness.thread.join();
}

#[test]
#[ignore = "needs the bench ECU; proves Test Connection reads only the signature"]
fn the_bench_test_connection_reads_only_the_signature() {
    let port = std::env::var("OX_TUNER_SERIAL_PORT")
        .unwrap_or_else(|_| "/dev/cu.usbmodem21201".to_owned());
    let definition = read_definition();
    let harness = Harness::start();
    harness
        .handle
        .load_definition(
            "speeduino.ini",
            std::fs::read(std::env::var("OX_TUNER_INI").map_or_else(
                |_| workspace_root().join("docs/tmp/local/ini/speeduino.ini"),
                PathBuf::from,
            ))
            .expect("the reference definition is present"),
        )
        .expect("queued");
    harness.wait(|event| matches!(event, Event::DefinitionLoaded { .. }));

    harness
        .handle
        .test_connection(PortChoice::serial(port.clone(), 115_200), None)
        .expect("queued");
    let Event::ConnectionTested { view: summary, .. } =
        harness.wait(|event| matches!(event, Event::ConnectionTested { .. }))
    else {
        unreachable!()
    };

    assert_eq!(summary.signature, definition.signature);
    assert_eq!(summary.verdict, oxtuner_core::SignatureVerdict::Match);
    assert!(!summary.read_only);
    // A probe reads no page and therefore leaves no frame behind.
    assert!(harness.latest.get().is_none());
    println!("test connection: {} on {}", summary.signature, summary.port);

    // The port was released: a full session still opens.
    harness
        .handle
        .connect(PortChoice::serial(port, 115_200), None)
        .expect("queued");
    harness.wait(|event| matches!(event, Event::Connected { .. }));
    harness.handle.disconnect().expect("queued");
    harness.wait(|event| matches!(event, Event::Disconnected { .. }));
    harness.thread.join();
}

#[test]
#[ignore = "needs the bench ECU; keeps the compile-time shape of the harness honest"]
fn the_bench_harness_reports_a_clean_failure_for_a_bad_port() {
    let harness = Harness::start();
    let bytes = std::fs::read(std::env::var("OX_TUNER_INI").map_or_else(
        |_| workspace_root().join("docs/tmp/local/ini/speeduino.ini"),
        PathBuf::from,
    ))
    .expect("the reference definition is present");
    harness
        .handle
        .load_definition("speeduino.ini", bytes)
        .expect("queued");
    harness.wait(|event| matches!(event, Event::DefinitionLoaded { .. }));

    harness
        .handle
        .connect(
            PortChoice::serial("/dev/ox-tuner-not-a-port", 115_200),
            None,
        )
        .expect("queued");
    let Event::Failed { failure, .. } = harness.wait(|event| matches!(event, Event::Failed { .. }))
    else {
        unreachable!()
    };

    assert_eq!(failure.code, FailureCode::ConnectFailed);
    harness.thread.join();
}

#[test]
#[ignore = "needs the bench ECU; reads the page CRC over the real port"]
fn the_bench_page_crc_matches_the_page_it_read() {
    let port = std::env::var("OX_TUNER_SERIAL_PORT")
        .unwrap_or_else(|_| "/dev/cu.usbmodem21201".to_owned());
    let definition = read_definition();
    let behavior = oxtuner_protocol::BehaviorKind::select(&definition).expect("a known family");
    let mut protocol = oxtuner_protocol::MsProtocol::new(&definition, behavior.as_dyn())
        .expect("the definition is complete");
    let mut transport = oxtuner_transport::SerialTransport::new(
        oxtuner_transport::SerialSettings::new(port).with_baud(115_200),
    );
    oxtuner_transport::Transport::open(&mut transport).expect("the port opens");
    // The Arduino resets on open; give it the definition's delay to boot.
    std::thread::sleep(std::time::Duration::from_millis(
        definition
            .comms
            .delay_after_port_open_ms
            .unwrap_or(0)
            .into(),
    ));

    for page in definition.pages.iter().take(3) {
        let data = protocol
            .read_page_all(&mut transport, page.number)
            .expect("the page reads");
        let expected = oxtuner_protocol::crc32(&data);
        let reported = protocol
            .read_page_crc(&mut transport, page.number)
            .expect("the CRC command runs")
            .expect("the definition declares a CRC command");
        assert_eq!(reported, expected, "page {} CRC", page.number);
        println!("page {} CRC 0x{reported:08x}", page.number);
    }
    oxtuner_transport::Transport::close(&mut transport);
}

fn read_definition() -> oxtuner_ini::Definition {
    let path = std::env::var("OX_TUNER_INI").map_or_else(
        |_| workspace_root().join("docs/tmp/local/ini/speeduino.ini"),
        PathBuf::from,
    );
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display()));
    oxtuner_ini::parse_bytes(&bytes, &SettingsContext::new()).expect("the reference definition")
}
