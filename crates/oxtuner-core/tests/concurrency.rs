//! Concurrency invariants around the realtime path and shutdown.
//!
//! Two promises are checked here rather than only documented: a UI that stops
//! reading events cannot stall acquisition (ADR-0021), and a shutdown still
//! completes when nobody is draining events (ADR-0005 rule 5).
//!
//! The link is the simulator through the application seam, so the code under
//! test is the production command and poll path.

use std::time::{Duration, Instant};

use oxtuner_core::{
    BlockingEventStream, CoreConfig, CoreError, CoreHandle, CoreThread, Event, FailureCode,
    LatestFrame, OpId, start,
};
use oxtuner_sim::{Fault, FaultRule, SimConfig, SimOp};

mod support;

const WAIT: Duration = Duration::from_secs(10);

const FIXTURE: &str = "\
[MegaTune]
signature = \"speeduino 202501\"
queryCommand = \"Q\"

[Constants]
endianness = little
nPages = 2
pageSize = 12, 8
page = 1
pageReadCommand = \"p%2i%2o%2c\", \"p%2i%2o%2c\"
burnCommand = \"b%2i\", \"\"
blockingFactor = 8
blockReadTimeout = 500
pageActivationDelay = 1
reqFuel = scalar, U08, 0, \"ms\", 0.1, 0, 0, 25.5, 1

[OutputChannels]
ochGetCommand = \"r%2o%2c\"
ochBlockSize = 4
rpm = scalar, U16, 0, \"rpm\", 1.0, 0.0
map = scalar, U08, 2, \"kPa\", 1.0, 0.0
";

/// A connected, polling session whose UI has stopped reading events.
struct StalledUi {
    handle: CoreHandle,
    thread: CoreThread,
    events: BlockingEventStream,
    latest: LatestFrame,
    history: oxtuner_core::ChartHistory,
    frames: oxtuner_core::BlockingFrameWatch,
}

impl StalledUi {
    fn start() -> Self {
        let parts = start(&CoreConfig::default()).expect("the core starts");
        let mut harness = Self {
            handle: parts.handle,
            thread: parts.thread,
            events: parts.events.into_blocking(),
            latest: parts.latest,
            history: parts.history,
            frames: parts.frames.into_blocking(),
        };
        harness.bring_up();
        harness
    }

    fn wait_for(&self, op: OpId) -> Event {
        let deadline = Instant::now() + WAIT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Some(event) = self.events.recv_timeout(left) else {
                panic!("no answer for {op} before the deadline");
            };
            if event.op() == Some(op) {
                return event;
            }
        }
    }

    /// Load, connect, read and start polling, consuming the answers as a UI
    /// would.
    fn bring_up(&mut self) {
        let op = self
            .handle
            .load_definition("speeduino.ini", FIXTURE.as_bytes())
            .expect("queued");
        assert!(
            matches!(self.wait_for(op), Event::DefinitionLoaded { .. }),
            "the definition must load"
        );

        let op = self
            .handle
            .connect(support::simulator(), None)
            .expect("queued");
        assert!(
            matches!(self.wait_for(op), Event::Connected { .. }),
            "the simulator must connect"
        );

        let op = self.handle.read_all().expect("queued");
        assert!(
            matches!(self.wait_for(op), Event::PagesRead { .. }),
            "every page must be read"
        );

        let op = self.handle.start_realtime(10).expect("queued");
        assert!(
            matches!(
                self.wait_for(op),
                Event::RealtimeState { running: true, .. }
            ),
            "polling must start"
        );
    }

    fn close(self) {
        let _ = self.handle.shutdown();
        self.thread.join();
    }
}

#[test]
fn a_stalled_ui_keeps_acquiring_frames() {
    let ui = StalledUi::start();

    // From here on the UI reads nothing: no event consumer, no repaint.
    let before = ui.history.stats().pushed;
    let deadline = Instant::now() + WAIT;
    while ui.history.stats().pushed < before + 5 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }

    let stats = ui.history.stats();
    assert!(
        stats.pushed >= before + 5,
        "acquisition stalled while the UI was not reading: {stats:?}"
    );
    assert!(
        ui.latest.get().is_some(),
        "the newest frame is readable without the event queue"
    );

    // A wake is waiting: the UI would repaint as soon as it looked.
    assert!(
        ui.frames.wait(Duration::from_secs(2)),
        "the frame wake never arrived"
    );

    // The point of the whole exercise: none of those frames entered the event
    // queue, so a stalled consumer cannot back the core up.
    assert!(
        ui.events.try_recv().is_none(),
        "a realtime frame reached the event queue"
    );

    // And the core still answers a command after all those frames.
    let op = ui.handle.stop_realtime().expect("queued");
    let answer = ui
        .events
        .recv_timeout(WAIT)
        .expect("the core answered after polling");
    assert_eq!(
        answer.op(),
        Some(op),
        "expected the stop answer, got {answer:?}"
    );

    ui.close();
}

#[test]
fn a_command_arriving_during_a_reconnect_is_answered_first() {
    // The link dies, the core backs off for 500 ms. A command must not wait
    // behind that backoff: it is answered while the link is still down.
    let parts = start(&CoreConfig::default()).expect("the core starts");
    let events = parts.events.into_blocking();

    let op = parts
        .handle
        .load_definition("speeduino.ini", FIXTURE.as_bytes())
        .expect("queued");
    let mut seen = false;
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline && !seen {
        let Some(event) = events.recv_timeout(WAIT) else {
            panic!("the definition was never loaded");
        };
        seen = event.op() == Some(op);
    }
    assert!(seen, "the definition was never loaded");

    let op = parts
        .handle
        .connect(
            support::simulator_with(SimConfig {
                faults: vec![FaultRule::new(SimOp::Realtime, Fault::Disconnect, 1)],
                ..SimConfig::default()
            }),
            None,
        )
        .expect("queued");
    loop {
        let Some(event) = events.recv_timeout(WAIT) else {
            panic!("the connection never answered");
        };
        if event.op() == Some(op) {
            assert!(matches!(event, Event::Connected { .. }), "{event:?}");
            break;
        }
    }

    let op = parts.handle.start_realtime(10).expect("queued");
    loop {
        let Some(event) = events.recv_timeout(WAIT) else {
            panic!("polling never started");
        };
        if event.op() == Some(op) {
            break;
        }
    }

    // The armed fault kills the link on the next poll.
    loop {
        let Some(event) = events.recv_timeout(WAIT) else {
            panic!("the link never went down");
        };
        if matches!(event, Event::ConnectionLost { .. }) {
            break;
        }
    }

    let read = parts.handle.read_all().expect("queued");
    let deadline = Instant::now() + WAIT;
    loop {
        assert!(Instant::now() < deadline, "the read was never answered");
        let Some(event) = events.recv_timeout(WAIT) else {
            panic!("the read was never answered");
        };
        assert!(
            !matches!(event, Event::Reconnected { .. }),
            "the command waited for the reconnect instead of being answered"
        );
        if event.op() == Some(read) {
            match event {
                Event::Failed { failure, .. } => {
                    assert_eq!(failure.code, FailureCode::NotConnected, "{failure:?}");
                }
                other => panic!("expected a not-connected failure, got {other:?}"),
            }
            break;
        }
    }

    let _ = parts.handle.shutdown();
    parts.thread.join();
}

#[test]
fn shutdown_is_bounded_when_the_ui_stops_reading_with_a_full_event_queue() {
    // The case the capacity floor cannot cover: the core keeps producing
    // answers over time, so a consumer that never reads fills the event queue
    // and the core blocks handing one back. Before ADR-0023 that blocked the
    // join forever; the stop flag now makes the wait finite and the lost
    // events are counted.
    let parts = start(&CoreConfig {
        command_capacity: 4,
        event_capacity: 1,
        ..CoreConfig::default()
    })
    .expect("the core starts");
    let oxtuner_core::CoreParts {
        handle,
        events,
        thread,
        ..
    } = parts;

    let mut accepted = Vec::new();
    let mut refusals = 0;
    let mut attempts = 0;
    // Keep offering work until the core is clearly blocked handing answers
    // back: refusals are what a full command queue looks like while a send to
    // the event queue is waiting.
    while refusals < 50 && attempts < 2_000 {
        match handle.load_definition(format!("full-{attempts}.ini"), FIXTURE.as_bytes()) {
            Ok(op) => accepted.push(op),
            Err(CoreError::QueueFull) => {
                refusals += 1;
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(other) => panic!("unexpected send error: {other}"),
        }
        attempts += 1;
    }
    assert!(
        accepted.len() > 6,
        "the core should have taken more work than one event-queue capacity \
         (accepted {})",
        accepted.len()
    );
    assert!(
        refusals > 0,
        "the command queue should have been full at some point"
    );

    // Nobody reads a single event until the join has returned. The join runs on
    // a watchdog so a regression fails the test instead of hanging it.
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let joiner = std::thread::spawn(move || {
        let report = thread.join();
        let _ = done_tx.send(report);
    });
    let report = done_rx.recv_timeout(Duration::from_secs(5)).expect(
        "the join never finished: a stalled consumer must not be able to keep the core alive",
    );
    joiner.join().expect("the joining thread");

    // Every accepted command was handled or rejected, and no answer vanished
    // without being counted.
    assert_eq!(
        report.handled + report.rejected,
        u64::try_from(accepted.len()).unwrap_or(u64::MAX),
        "every accepted command must be handled or rejected: {report:?}"
    );

    let events = events.into_blocking();
    let mut delivered = 0u64;
    let mut answered = std::collections::HashSet::new();
    while let Some(event) = events.recv_timeout(Duration::from_millis(200)) {
        delivered += 1;
        match event {
            Event::Stopped => break,
            other => {
                if let Some(op) = other.op() {
                    assert!(answered.insert(op), "{op} was delivered twice");
                }
            }
        }
    }

    // Every accepted command produced exactly one event (its answer or a
    // `Rejected`), plus the single final `Stopped`; each of those either
    // reached the UI or was counted as dropped.
    let expected = u64::try_from(accepted.len()).unwrap_or(u64::MAX) + 1;
    assert_eq!(
        delivered + report.dropped_events,
        expected,
        "delivered {delivered} + dropped {} should account for {expected} events ({report:?})",
        report.dropped_events
    );
    assert!(
        report.dropped_events > 0,
        "a stalled consumer with a tiny queue should lose some events, not none"
    );
    drop(handle);
}
