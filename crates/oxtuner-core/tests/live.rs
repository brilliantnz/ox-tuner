//! The M5/M6 gates: the core's live session against the virtual ECU.
//!
//! No hardware is involved. The same `oxtuner-core` command path the app uses
//! connects to `oxtuner-sim`, reads every page, polls realtime values, records a
//! trace and survives an injected link loss — so these tests exercise the
//! production session code, not a mock of it.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use oxtuner_core::{
    BlockingEventStream, CoreConfig, CoreHandle, CoreThread, Event, FailureCode, LatestFrame, OpId,
    ProbeQuery, SignatureVerdict,
};
use oxtuner_ini::{SettingsContext, parse_str};
use oxtuner_protocol::{BehaviorKind, LinkMode, MsProtocol, Protocol};
use oxtuner_sim::{Fault, FaultRule, SimConfig, SimOp, SimTransport};
use oxtuner_transport::{ReplayTransport, Trace, Transport};

mod support;

const FIXTURE: &str = "\
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
burnCommand = \"b%2i\", \"\"
blockingFactor = 8
blockReadTimeout = 500
pageActivationDelay = 1
tsWriteBlocks = on
messageEnvelopeFormat = msEnvelope_1.0
reqFuel = scalar, U08, 0, \"ms\", 0.1, 0, 0, 25.5, 1

[OutputChannels]
ochGetCommand = \"r\\$tsCanId\\x30%2o%2c\"
ochBlockSize = 8
rpm = scalar, U16, 0, \"rpm\", 1.0, 0.0
tps = scalar, U08, 3, \"%\", 0.5, 0.0
";

/// A core with its blocking event stream.
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

    fn load(&self) -> OpId {
        self.handle
            .load_definition("speeduino.ini", FIXTURE.as_bytes())
            .expect("queued")
    }

    fn connect(&self, config: SimConfig) -> OpId {
        self.handle
            .connect(support::simulator_with(config), None)
            .expect("queued")
    }

    /// Waits for the first event matching `wanted`.
    fn wait(&self, wanted: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(event) = self.events.recv_timeout(left) else {
                assert!(
                    !self.events.is_closed(),
                    "the core stopped before answering"
                );
                panic!("no matching event before the deadline");
            };
            if wanted(&event) {
                return event;
            }
        }
    }

    /// Collects whatever arrives within `window`.
    fn drain_for(&self, window: Duration) -> Vec<Event> {
        let deadline = Instant::now() + window;
        let mut events = Vec::new();

        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            let Some(event) = self.events.recv_timeout(left) else {
                break;
            };

            events.push(event);
        }

        events
    }

    /// Waits for the definition load.
    fn wait_loaded(&self) {
        self.wait(|event| matches!(event, Event::DefinitionLoaded { .. }));
    }

    fn wait_connected(&self) -> oxtuner_core::ConnectionView {
        match self.wait(|event| matches!(event, Event::Connected { .. })) {
            Event::Connected { view: summary, .. } => summary,
            _ => unreachable!(),
        }
    }
}

#[test]
fn connecting_to_the_simulator_reports_the_signature() {
    let harness = Harness::start();
    harness.load();
    harness.wait_loaded();

    harness.connect(SimConfig::default());
    let summary = harness.wait_connected();

    assert_eq!(summary.signature, "speeduino 202501");
    assert_eq!(summary.verdict, SignatureVerdict::Match);
    assert!(!summary.read_only);
    assert!(summary.custom);
    // The fixture declares the message envelope, so that is what the session
    // picked; the legacy path is still selectable per connection.
    assert_eq!(summary.framing, "envelope");
    assert_eq!(summary.pages, 2);

    harness.thread.join();
}

#[test]
fn a_probe_reads_a_signature_without_a_loaded_definition() {
    let harness = Harness::start();

    // No definition is loaded: the probe carries the query and reply shape.
    harness
        .handle
        .probe_firmware(
            support::simulator(),
            vec![ProbeQuery::new(
                oxtuner_core::FirmwareProbe::Speeduino,
                oxtuner_core::LinkMode::Legacy,
            )],
        )
        .expect("queued");

    let Event::FirmwareProbed {
        probe,
        signature,
        baud,
        ..
    } = harness.wait(|event| matches!(event, Event::FirmwareProbed { .. }))
    else {
        unreachable!()
    };

    assert_eq!(probe, oxtuner_core::FirmwareProbe::Speeduino);
    assert!(signature.starts_with("speeduino"), "{signature}");
    assert_eq!(baud, None, "a custom link has no baud rate");

    // A probe is not a session: a real connect still works afterwards.
    harness.load();
    harness.wait_loaded();
    harness.connect(SimConfig::default());
    let summary = harness.wait_connected();
    assert_eq!(summary.signature, "speeduino 202501");

    harness.thread.join();
}

#[test]
fn a_probe_reads_an_enveloped_signature() {
    let harness = Harness::start();

    harness
        .handle
        .probe_firmware(
            support::simulator(),
            vec![ProbeQuery::new(
                oxtuner_core::FirmwareProbe::Speeduino,
                oxtuner_core::LinkMode::Envelope,
            )],
        )
        .expect("queued");

    let Event::FirmwareProbed {
        framing, signature, ..
    } = harness.wait(|event| matches!(event, Event::FirmwareProbed { .. }))
    else {
        unreachable!()
    };

    assert_eq!(framing, oxtuner_core::LinkMode::Envelope);
    assert!(signature.starts_with("speeduino"), "{signature}");

    harness.thread.join();
}

#[test]
fn a_probe_reports_whatever_text_a_device_answers_with() {
    // The core has no definition to judge an answer against, so a chatty
    // adapter, an echo or an NMEA device produces a signature just like an ECU.
    // Classifying that text is the job of the layer that owns the registry; the
    // core must not swallow the answer.
    const NMEA: &str = "$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47";

    let harness = Harness::start();

    harness
        .handle
        .probe_firmware(
            support::answering_probe(NMEA),
            vec![ProbeQuery::new(
                oxtuner_core::FirmwareProbe::Speeduino,
                oxtuner_core::LinkMode::Legacy,
            )],
        )
        .expect("queued");

    let Event::FirmwareProbed { signature, .. } =
        harness.wait(|event| matches!(event, Event::FirmwareProbed { .. }))
    else {
        unreachable!()
    };

    assert_eq!(signature, NMEA);

    harness.thread.join();
}

#[test]
fn a_probe_on_a_silent_link_reports_that_nothing_answered() {
    let harness = Harness::start();

    harness
        .handle
        .probe_firmware(
            support::silent(),
            vec![ProbeQuery::new(
                oxtuner_core::FirmwareProbe::Speeduino,
                oxtuner_core::LinkMode::Legacy,
            )],
        )
        .expect("queued");

    let Event::Failed { failure, .. } = harness.wait(|event| matches!(event, Event::Failed { .. }))
    else {
        unreachable!()
    };

    assert_eq!(failure.code, FailureCode::FirmwareNotDetected);

    let report = harness.thread.join();
    assert_eq!(report.failures, 1, "a probe miss is a failed command");
}

#[test]
fn a_probe_group_opens_the_port_once_and_stops_at_the_first_answer() {
    let harness = Harness::start();
    let (port, opens) = support::counting_probe(1);

    // The first query is met with silence, so the group has to reach the
    // second family on the same open port. The third would answer too, which is
    // what proves the group stopped.
    harness
        .handle
        .probe_firmware(
            port,
            vec![
                ProbeQuery::new(oxtuner_core::FirmwareProbe::Speeduino, LinkMode::Legacy),
                ProbeQuery::new(oxtuner_core::FirmwareProbe::Rusefi, LinkMode::Legacy),
                ProbeQuery::new(oxtuner_core::FirmwareProbe::Rusefi, LinkMode::Envelope),
            ],
        )
        .expect("queued");

    let Event::ProbeStarted { probe, .. } =
        harness.wait(|event| matches!(event, Event::ProbeStarted { .. }))
    else {
        unreachable!()
    };
    assert_eq!(
        probe,
        oxtuner_core::FirmwareProbe::Speeduino,
        "the group asks its first query first"
    );

    let Event::FirmwareProbed { probe, .. } =
        harness.wait(|event| matches!(event, Event::FirmwareProbed { .. }))
    else {
        unreachable!()
    };
    assert_eq!(
        probe,
        oxtuner_core::FirmwareProbe::Rusefi,
        "the silent query moved the group on to the second family"
    );
    assert_eq!(
        opens.load(Ordering::Relaxed),
        1,
        "one open serves the whole group"
    );
    assert!(
        !harness
            .drain_for(Duration::from_millis(200))
            .iter()
            .any(|event| matches!(event, Event::ProbeStarted { .. })),
        "the group stopped at the answer: the third query was never asked"
    );

    harness.thread.join();
}

#[test]
fn a_silent_group_reports_one_failure_for_several_queries() {
    let harness = Harness::start();
    let (port, _opens) = support::counting_probe(usize::MAX);

    harness
        .handle
        .probe_firmware(
            port,
            vec![
                ProbeQuery::new(oxtuner_core::FirmwareProbe::Speeduino, LinkMode::Legacy),
                ProbeQuery::new(oxtuner_core::FirmwareProbe::Rusefi, LinkMode::Legacy),
            ],
        )
        .expect("queued");

    let mut started = 0;
    loop {
        match harness
            .wait(|event| matches!(event, Event::ProbeStarted { .. } | Event::Failed { .. }))
        {
            Event::ProbeStarted { .. } => started += 1,
            Event::Failed { failure, .. } => {
                assert_eq!(failure.code, FailureCode::FirmwareNotDetected);
                break;
            }
            _ => {}
        }
    }

    assert_eq!(started, 2, "every query of the group was asked");
    assert!(
        !harness
            .drain_for(Duration::from_millis(200))
            .iter()
            .any(|event| matches!(event, Event::Failed { .. })),
        "a group reports one failure, not one per query"
    );

    let report = harness.thread.join();
    assert_eq!(report.failures, 1);
}

#[test]
fn a_probe_is_refused_while_a_session_is_live() {
    let harness = Harness::start();
    harness.load();
    harness.wait_loaded();
    harness.connect(SimConfig::default());
    let _ = harness.wait_connected();

    harness
        .handle
        .probe_firmware(
            support::simulator(),
            vec![ProbeQuery::new(
                oxtuner_core::FirmwareProbe::Rusefi,
                oxtuner_core::LinkMode::Legacy,
            )],
        )
        .expect("queued");

    let Event::Failed { failure, .. } = harness.wait(|event| matches!(event, Event::Failed { .. }))
    else {
        unreachable!()
    };

    assert_eq!(failure.code, FailureCode::ConnectFailed);
    assert!(failure.message.contains("disconnect the ECU"));

    let report = harness.thread.join();
    assert_eq!(report.failures, 1, "a refused probe is a failed command");
}

#[test]
fn the_core_probe_list_matches_the_protocol_registry() {
    assert_eq!(
        oxtuner_core::FirmwareProbe::ALL.map(oxtuner_core::FirmwareProbe::spec),
        oxtuner_protocol::PROBES,
        "the core's probe list and the protocol registry must match, in order"
    );
}

#[test]
fn connecting_without_a_definition_is_refused() {
    let harness = Harness::start();

    harness.connect(SimConfig::default());
    let Event::Failed { failure, .. } = harness.wait(|event| matches!(event, Event::Failed { .. }))
    else {
        unreachable!()
    };

    assert_eq!(failure.code, FailureCode::ConnectFailed);
    assert!(failure.message.contains("open a project"));

    harness.thread.join();
}

#[test]
fn connecting_reads_the_optional_firmware_text() {
    let harness = Harness::start();
    let fixture = FIXTURE.replace(
        "queryCommand = \"Q\"",
        "queryCommand = \"Q\"\nversionInfo = \"S\"",
    );
    harness
        .handle
        .load_definition("versioned.ini", fixture.as_bytes())
        .expect("queued");
    harness.wait_loaded();

    harness.connect(SimConfig {
        version_text: Some("Speeduino 2025.01.7".to_owned()),
        ..SimConfig::default()
    });
    let summary = harness.wait_connected();

    assert_eq!(
        summary.version_text.as_deref(),
        Some("Speeduino 2025.01.7"),
        "the version text is separate from the signature"
    );
    assert_eq!(summary.signature, "speeduino 202501");

    harness.thread.join();
}

#[test]
fn a_mismatched_signature_locks_the_session_read_only() {
    let harness = Harness::start();
    harness.load();
    harness.wait_loaded();

    harness.connect(SimConfig {
        signature: Some("speeduino 999999".to_owned()),
        ..SimConfig::default()
    });
    let summary = harness.wait_connected();

    assert_eq!(summary.verdict, SignatureVerdict::Mismatch);
    assert!(summary.read_only);
    assert!(
        summary
            .read_only_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("speeduino 999999"))
    );

    harness.thread.join();
}

#[test]
fn read_all_reports_every_declared_page() {
    let harness = Harness::start();
    harness.load();
    harness.wait_loaded();
    harness.connect(SimConfig::default());
    harness.wait_connected();

    harness.handle.read_all().expect("queued");
    let Event::PagesRead {
        report: summary, ..
    } = harness.wait(|event| matches!(event, Event::PagesRead { .. }))
    else {
        unreachable!()
    };

    assert_eq!(summary.pages, 2);
    assert_eq!(summary.bytes, 20);
    assert!(summary.constants > 0);
    assert_eq!(summary.skipped, 0);
    assert!(summary.signature_verified);

    harness.thread.join();
}

#[test]
fn realtime_values_arrive_through_the_latest_frame() {
    let harness = Harness::start();
    harness.load();
    harness.wait_loaded();
    harness.connect(SimConfig::default());
    harness.wait_connected();
    harness.handle.read_all().expect("queued");
    harness.wait(|event| matches!(event, Event::PagesRead { .. }));

    // Polling starts after the read; the frame is shared, not queued.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut frame = None;
    while Instant::now() < deadline {
        if let Some(latest) = harness.latest.get()
            && !latest.is_empty()
        {
            frame = Some(latest);
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let frame = frame.expect("a realtime frame arrived");
    assert!(frame.value("rpm").is_some());
    assert!(
        frame.value("rpm").expect("rpm is decoded").value >= 900.0,
        "the simulator's rpm must move"
    );

    harness.thread.join();
}

#[test]
fn a_lost_link_is_reported_and_the_core_reconnects() {
    let harness = Harness::start();
    let fixture = FIXTURE.replace(
        "queryCommand = \"Q\"",
        "queryCommand = \"Q\"\nversionInfo = \"S\"",
    );
    harness
        .handle
        .load_definition("versioned.ini", fixture.as_bytes())
        .expect("queued");
    harness.wait_loaded();
    harness.connect(SimConfig {
        version_text: Some("Speeduino 2025.01.7".to_owned()),
        faults: vec![FaultRule::new(SimOp::ReadPage, Fault::Disconnect, 1)],
        ..SimConfig::default()
    });
    let connected = harness.wait_connected();
    assert_eq!(
        connected.version_text.as_deref(),
        Some("Speeduino 2025.01.7")
    );

    harness.handle.read_all().expect("queued");

    // The page read dies, the command is answered, and the session says so.
    let Event::Failed { failure, .. } = harness.wait(|event| matches!(event, Event::Failed { .. }))
    else {
        unreachable!()
    };
    assert_eq!(failure.code, FailureCode::ReadFailed);
    harness.wait(|event| matches!(event, Event::ConnectionLost { .. }));

    // The core retries with backoff and reports the ECU again.
    let Event::Reconnected { view: summary } =
        harness.wait(|event| matches!(event, Event::Reconnected { .. }))
    else {
        unreachable!()
    };
    assert_eq!(summary.signature, "speeduino 202501");
    assert_eq!(
        summary.version_text.as_deref(),
        Some("Speeduino 2025.01.7"),
        "a reconnect keeps the version text the controller already reported"
    );

    harness.thread.join();
}

#[test]
fn a_recorded_session_contains_only_read_commands_and_replays() {
    let harness = Harness::start();
    harness.load();
    harness.wait_loaded();
    harness.connect(SimConfig::default());
    harness.wait_connected();

    harness.handle.read_all().expect("queued");
    harness.wait(|event| matches!(event, Event::PagesRead { .. }));
    // Polling starts after the read; stop it so the recorded trace is a clean
    // read session and the replay drives exactly the recorded commands.
    harness.handle.stop_realtime().expect("queued");
    harness.wait(|event| matches!(event, Event::RealtimeState { running: false, .. }));

    let directory = std::env::temp_dir().join(format!(
        "oxtuner-live-{}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    let path = directory.join("session.trace");
    harness.handle.save_trace(&path).expect("queued");
    let Event::TraceSaved { frames, .. } =
        harness.wait(|event| matches!(event, Event::TraceSaved { .. }))
    else {
        unreachable!()
    };
    assert!(frames > 0);

    let trace = Trace::load(&path).expect("the trace parses");
    let _ = std::fs::remove_dir_all(&directory);

    // No-write proof: every request on the wire is one of the definition's read
    // commands. With the message envelope the frame starts with its length, so
    // the command byte is the first payload byte.
    for frame in trace
        .frames()
        .iter()
        .filter(|frame| frame.direction == oxtuner_transport::Direction::Tx)
    {
        let command = match oxtuner_protocol::Frame::parse(&frame.bytes) {
            Ok(frame) => *frame.payload.first().expect("a frame has a command"),
            Err(_) => *frame.bytes.first().expect("a frame has bytes"),
        };
        assert!(
            matches!(command, b'Q' | b'p' | b'r'),
            "a write-ish command reached the wire: 0x{command:02x}"
        );
    }

    // Replay through the same protocol: the recorded conversation answers
    // without hardware, deterministically.
    let definition = parse_str(FIXTURE, &SettingsContext::new()).expect("the fixture parses");
    let behavior = BehaviorKind::select(&definition).expect("a known family");
    let mut replay = ReplayTransport::new(trace);
    replay.open().expect("replay opens");
    let mut protocol = MsProtocol::new(&definition, behavior.as_dyn())
        .expect("the fixture is complete")
        .with_link_mode(LinkMode::Envelope);
    assert_eq!(
        protocol.signature(&mut replay).expect("replayed signature"),
        "speeduino 202501"
    );
    let page = protocol
        .read_page_all(&mut replay, 1)
        .expect("replayed page 1");
    assert_eq!(page.len(), 12);

    // The same definition and seed produce the same page in a fresh simulator.
    let fresh = SimTransport::new(&definition, SimConfig::default()).expect("simulates");
    assert_eq!(Some(page), fresh.ram(1).map(<[u8]>::to_vec));

    harness.thread.join();
}

#[test]
fn disconnecting_releases_the_frame() {
    let harness = Harness::start();
    harness.load();
    harness.wait_loaded();
    harness.connect(SimConfig::default());
    harness.wait_connected();
    harness.handle.read_all().expect("queued");
    harness.wait(|event| matches!(event, Event::PagesRead { .. }));

    let deadline = Instant::now() + Duration::from_secs(5);
    while harness.latest.get().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(harness.latest.get().is_some());

    harness.handle.disconnect().expect("queued");
    harness.wait(|event| matches!(event, Event::Disconnected { .. }));

    assert!(harness.latest.get().is_none());

    harness.thread.join();
}

#[test]
fn the_port_list_answers_without_hardware() {
    let harness = Harness::start();

    harness.handle.list_ports().expect("queued");
    let Event::PortsListed { ports, .. } =
        harness.wait(|event| matches!(event, Event::PortsListed { .. }))
    else {
        unreachable!()
    };

    // The list is machine-dependent; what must hold is that every entry names
    // a path and that the command answered instead of hanging.
    for port in ports {
        assert_ne!(port.name, "", "every listed port must have a path");
    }

    harness.thread.join();
}

#[test]
fn a_legacy_connection_can_be_requested_explicitly() {
    let harness = Harness::start();
    harness.load();
    harness.wait_loaded();

    harness
        .handle
        .connect(support::simulator(), Some(LinkMode::Legacy))
        .expect("queued");
    let summary = harness.wait_connected();

    assert_eq!(summary.framing, "legacy");

    harness.thread.join();
}

#[test]
fn the_simulator_config_can_preload_a_page() {
    let harness = Harness::start();
    harness.load();
    harness.wait_loaded();
    harness.connect(SimConfig {
        pages: vec![(1, vec![0xAA; 12])],
        ..SimConfig::default()
    });
    harness.wait_connected();

    harness.handle.read_all().expect("queued");
    harness.wait(|event| matches!(event, Event::PagesRead { .. }));

    // The image is private to the core; a store here would only prove the
    // simulator kept its own copy. The read summary proving every page was
    // accepted is the observable part.
    harness.thread.join();
}

#[test]
fn a_second_connect_is_refused_while_the_first_is_open() {
    let harness = Harness::start();
    harness.load();
    harness.wait_loaded();
    harness.connect(SimConfig::default());
    harness.wait_connected();

    harness.connect(SimConfig::default());
    let Event::Failed { failure, .. } = harness.wait(|event| matches!(event, Event::Failed { .. }))
    else {
        unreachable!()
    };
    assert_eq!(failure.code, FailureCode::ConnectFailed);
    assert!(failure.message.contains("already open"));

    harness.handle.disconnect().expect("queued");
    harness.wait(|event| matches!(event, Event::Disconnected { .. }));
    harness.thread.join();
}

#[test]
fn a_test_connection_reads_the_signature_and_releases_the_port() {
    let harness = Harness::start();
    harness.load();
    harness.wait_loaded();

    harness
        .handle
        .test_connection(support::simulator(), None)
        .expect("queued");
    let Event::ConnectionTested { view: summary, .. } =
        harness.wait(|event| matches!(event, Event::ConnectionTested { .. }))
    else {
        unreachable!()
    };

    assert_eq!(summary.signature, "speeduino 202501");
    assert_eq!(summary.verdict, SignatureVerdict::Match);
    assert!(!summary.read_only);

    // The probe is not a session: a real connection can still be opened.
    harness.connect(SimConfig::default());
    let connected = harness.wait_connected();
    assert_eq!(connected.verdict, SignatureVerdict::Match);

    harness.thread.join();
}

#[test]
fn a_test_connection_reports_a_mismatched_signature_as_read_only() {
    let harness = Harness::start();
    harness.load();
    harness.wait_loaded();

    harness
        .handle
        .test_connection(
            support::simulator_with(SimConfig {
                signature: Some("speeduino 999999".to_owned()),
                ..SimConfig::default()
            }),
            None,
        )
        .expect("queued");
    let Event::ConnectionTested { view: summary, .. } =
        harness.wait(|event| matches!(event, Event::ConnectionTested { .. }))
    else {
        unreachable!()
    };

    assert_eq!(summary.verdict, SignatureVerdict::Mismatch);
    assert!(summary.read_only);
    assert!(summary.read_only_reason.is_some());

    harness.thread.join();
}

#[test]
fn a_test_connection_while_connected_is_refused() {
    let harness = Harness::start();
    harness.load();
    harness.wait_loaded();
    harness.connect(SimConfig::default());
    harness.wait_connected();

    harness
        .handle
        .test_connection(support::simulator(), None)
        .expect("queued");
    let Event::Failed { failure, .. } = harness.wait(|event| matches!(event, Event::Failed { .. }))
    else {
        unreachable!()
    };
    assert_eq!(failure.code, FailureCode::ConnectFailed);
    assert!(failure.message.contains("disconnect"));

    harness.handle.disconnect().expect("queued");
    harness.wait(|event| matches!(event, Event::Disconnected { .. }));
    harness.thread.join();
}

#[test]
fn replacing_the_session_answers_an_in_flight_read() {
    let harness = Harness::start();
    harness.load();
    harness.wait_loaded();
    harness.connect(SimConfig {
        faults: vec![FaultRule::new(
            SimOp::ReadPage,
            Fault::Delay {
                delay: Duration::from_millis(300),
            },
            1,
        )],
        ..SimConfig::default()
    });
    harness.wait_connected();

    let read = harness.handle.read_all().expect("queued");
    // Replace the session while the read is still on the wire. Its command must
    // be answered, not dropped with the connection.
    harness.load();
    let Event::Failed { op, failure } =
        harness.wait(|event| matches!(event, Event::Failed { op, .. } if *op == read))
    else {
        unreachable!()
    };
    assert_eq!(op, read);
    assert_eq!(failure.code, FailureCode::ReadFailed);
    assert!(failure.message.contains("closed"));
    harness.wait(|event| matches!(event, Event::DefinitionLoaded { .. }));

    harness.thread.join();
}
