//! M8 pipeline against the simulator: the shared chart history and the CSV
//! session log.
//!
//! No UI is involved. The same `CoreParts` the app holds is read here, so the
//! ring-buffer accounting, the channel order and the logger thread are covered
//! end to end.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use oxtuner_core::{
    BlockingEventStream, ChartHistory, CoreConfig, CoreHandle, CoreParts, CoreThread, Event,
    LogStatus, OpId, start,
};

mod support;

const WAIT: Duration = Duration::from_secs(10);

const DEFINITION: &str = r#"
[MegaTune]
signature = "speeduino 202501"
queryCommand = "Q"

[Constants]
endianness = little
nPages = 1
pageSize = 16
page = 1
pageIdentifier = "\x00\x01"
pageReadCommand = "p%2i%2o%2c"
burnCommand = "b%2i"
blockingFactor = 16
reqFuel = scalar, U08, 12, "ms", 0.1, 0, 0, 25.5, 1
veTable = array, U08, 0, [2x2], "%", 1, 0, 0, 255, 0

[TableEditor]
table = veTableTbl, veMap, "VE Table", 1
  xBins = rpmBins, rpm
  yBins = mapBins, map
  zBins = veTable

[OutputChannels]
ochGetCommand = "r%2o%2c"
ochBlockSize = 4
rpm = scalar, U16, 0, "rpm", 1.0, 0.0
map = scalar, U08, 2, "kPa", 1.0, 0.0
"#;

const TUNE: &str = r#"<?xml version="1.0" encoding="ISO-8859-1"?>
<msq xmlns="http://www.msefi.com/:msq">
    <versionInfo fileFormat="5.0" firmwareInfo="Speeduino+2025.01.3" nPages="1" signature="speeduino 202501"/>
    <page number="0" size="16">
        <constant cols="2" digits="0" name="veTable" rows="2" units="%">10.0 20.0 30.0 40.0</constant>
        <constant name="reqFuel">12.0</constant>
    </page>
</msq>
"#;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "oxtuner-core-history-{label}-{}-{sequence}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(path.join("projectCfg")).expect("temp project");
        fs::write(path.join("projectCfg/mainController.ini"), DEFINITION).expect("ini");
        fs::write(path.join(oxtuner_project::TUNE_FILE), TUNE).expect("tune");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct Harness {
    handle: CoreHandle,
    thread: CoreThread,
    events: BlockingEventStream,
    history: ChartHistory,
    log: LogStatus,
}

impl Harness {
    fn start() -> Self {
        let CoreParts {
            handle,
            events,
            latest: _,
            frames: _,
            history,
            log,
            thread,
        } = start(CoreConfig::default()).expect("core starts");
        Self {
            handle,
            thread,
            events: events.into_blocking(),
            history,
            log,
        }
    }

    fn wait(&self, wanted: impl Fn(&Event) -> bool) -> Event {
        let deadline = Instant::now() + WAIT;
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

    fn wait_for_op(&self, op: OpId) -> Event {
        self.wait(|event| event.op() == Some(op))
    }

    fn open(&self, dir: &TempDir) {
        let op = self.handle.open_project(dir.path()).expect("queued");
        let Event::ProjectOpened { .. } = self.wait_for_op(op) else {
            panic!("project did not open");
        };
    }

    fn connect(&self) {
        let op = self
            .handle
            .connect(support::simulator(), None)
            .expect("queued");
        let Event::Connected { .. } = self.wait_for_op(op) else {
            panic!("did not connect");
        };
    }

    fn read_all(&self) {
        let op = self.handle.read_all().expect("queued");
        let Event::PagesRead { .. } = self.wait_for_op(op) else {
            panic!("pages did not read");
        };
    }
}

#[test]
fn realtime_frames_fill_the_shared_history() {
    let dir = TempDir::new("history");
    let core = Harness::start();
    core.open(&dir);
    core.connect();
    core.read_all();
    let op = core.handle.start_realtime(10).expect("queued");
    let Event::RealtimeState { running, .. } = core.wait_for_op(op) else {
        panic!("polling did not start");
    };
    assert!(running);

    let deadline = Instant::now() + WAIT;
    while core.history.stats().frames < 5 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }

    let stats = core.history.stats();
    assert!(stats.frames >= 5, "{stats:?}");
    assert_eq!(stats.pushed, stats.frames as u64 + stats.overruns);
    assert_eq!(stats.overruns, 0);

    let names: Vec<String> = core
        .history
        .channels()
        .iter()
        .map(|channel| channel.name.clone())
        .collect();
    assert_eq!(names, ["rpm", "map"]);
    let ids = vec!["rpm".to_owned(), "map".to_owned()];
    let newest = core.history.newest_time().expect("a frame");
    let nearest = core.history.nearest(&ids, newest);
    assert!(nearest.iter().all(|nearest| nearest.sample.is_some()));

    let _ = core.handle.stop_realtime();
    core.thread.join();
}

#[test]
fn a_session_log_writes_the_definition_order_and_stops_cleanly() {
    let dir = TempDir::new("log");
    let core = Harness::start();
    core.open(&dir);
    core.connect();
    core.read_all();

    let op = core.handle.start_realtime(10).expect("queued");
    let _ = core.wait_for_op(op);

    let log_dir = dir.path().join("logs");
    let op = core.handle.start_log(&log_dir).expect("queued");
    let Event::LogState { running, .. } = core.wait_for_op(op) else {
        panic!("log did not start");
    };
    assert!(running);

    let deadline = Instant::now() + WAIT;
    while core.log.frames() < 3 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }

    let op = core.handle.stop_log().expect("queued");
    let Event::LogState {
        running: false,
        summary: Some(summary),
        ..
    } = core.wait_for_op(op)
    else {
        panic!("log did not stop with a summary");
    };
    assert!(!core.log.is_running());
    assert!(summary.frames >= 3, "{summary:?}");
    assert_eq!(
        summary.dropped, 0,
        "the simulator keeps up at the default rate"
    );

    let text = fs::read_to_string(&summary.path).expect("log file");
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("Time,rpm,map"));
    let first = lines.next().expect("a data row");
    assert_eq!(first.split(',').count(), 3);
    assert!(summary.path.starts_with(&log_dir));

    core.thread.join();
}

#[test]
fn history_is_cleared_when_the_definition_changes() {
    let dir = TempDir::new("clear");
    let core = Harness::start();
    core.open(&dir);
    core.connect();
    core.read_all();
    assert!(!core.history.channels().is_empty());

    let op = core.handle.unload_definition().expect("queued");
    let _ = core.wait_for_op(op);

    assert!(core.history.channels().is_empty());
    assert_eq!(core.history.stats().frames, 0);

    core.thread.join();
}
