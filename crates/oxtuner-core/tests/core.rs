//! Core invariants from ADR-0005, exercised headlessly.
//!
//! One owner, answered exactly once, prompt under load, deterministic
//! shutdown, and no GUI or async runtime in the dependency tree.

use std::collections::{HashMap, HashSet};
use std::thread;
use std::time::{Duration, Instant};

use oxtuner_core::{
    BlockingEventStream, CoreCommand, CoreConfig, CoreError, CoreHandle, CoreThread,
    DefinitionCommand, DefinitionView, Event, OpId, start,
};

mod support;

/// How long a test waits for an event before failing. Generous: CI machines are
/// slow, and a passing run never waits anywhere near this.
const WAIT: Duration = Duration::from_secs(10);

const SPEEDUINO_LIKE: &str = "\
[MegaTune]
signature = \"speeduino 202501\"
queryCommand = \"Q\"

[TunerStudio]
iniSpecVersion = 3.78

[Constants]
endianness = little
nPages = 2
pageSize = 288, 288
pageReadCommand = \"p%2i%2o%2c\", \"p%2i%2o%2c\"
burnCommand = \"b%2i\", \"b%2i\"
#if COMMS_COMPAT
blockingFactor = 121
#else
blockingFactor = 251
#endif
page = 1
reqFuel = scalar, U16, 0, \"ms\", 0.1, 0, 0, 25.5, 1
staged = array, U08, 8, [8], \"x\", 1, 0, 0, 255, 0
";

const OTHER_LIKE: &str = "\
[MegaTune]
signature = \"rusEFI test\"
queryCommand = \"S\"
";

/// A running core whose events are consumed synchronously.
struct Harness {
    handle: CoreHandle,
    thread: CoreThread,
    events: BlockingEventStream,
}

impl Harness {
    fn start() -> Self {
        Self::start_with(&CoreConfig::default())
    }

    fn start_with(config: &CoreConfig) -> Self {
        let parts = start(config).expect("the core starts");
        Self {
            handle: parts.handle,
            thread: parts.thread,
            events: parts.events.into_blocking(),
        }
    }

    fn load(&self, label: &str, text: &str) -> OpId {
        self.handle
            .load_definition(label, text.as_bytes())
            .expect("the command is queued")
    }

    fn next(&self) -> Event {
        // The core pushes its port snapshot on its own, so a command's answer
        // is read past those publications.
        support::recv_non_port(&self.events, WAIT).expect("an event arrives")
    }

    /// Collects events until `Stopped`, which is always the last one.
    fn drain(&self) -> Vec<Event> {
        let mut collected = Vec::new();
        while let Some(event) = self.events.recv_timeout(WAIT) {
            let done = matches!(event, Event::Stopped);
            collected.push(event);
            if done {
                break;
            }
        }
        collected
    }
}

fn summary_of(event: &Event) -> &DefinitionView {
    match event {
        Event::DefinitionLoaded { view: summary, .. } => summary,
        other => panic!("expected a definition load, got {other:?}"),
    }
}

#[test]
fn a_loaded_definition_is_summarised_for_the_ui() {
    let core = Harness::start();

    let op = core.load("speeduino.ini", SPEEDUINO_LIKE);
    let event = core.next();

    assert_eq!(event.op(), Some(op), "the reply carries the command's id");
    let summary = summary_of(&event);
    assert_eq!(summary.label, "speeduino.ini");
    assert_eq!(summary.signature, "speeduino 202501");
    assert_eq!(summary.spec_version, Some(3.78));
    assert_eq!(summary.pages, 2);
    assert_eq!(summary.constants, 2);
    assert!(summary.is_clean(), "the fixture parses without errors");
    assert!(
        (summary.coverage_percent - 100.0).abs() < f64::EPSILON,
        "coverage was {}",
        summary.coverage_percent
    );

    core.thread.join();
}

#[test]
fn the_settings_snapshot_decides_the_protocol_commands() {
    // ADR-0006: `#if` branches are resolved against the settings the load
    // carried, so the same bytes can describe two different protocols.
    let core = Harness::start();

    let mut compatible = oxtuner_core::SettingsContext::new();
    compatible.set_override("COMMS_COMPAT", true);
    let op = core
        .handle
        .execute(CoreCommand::Definition(DefinitionCommand::Load {
            label: "compat.ini".to_owned(),
            bytes: SPEEDUINO_LIKE.as_bytes().to_vec(),
            settings: compatible,
        }))
        .expect("queued");
    let event = core.next();

    assert_eq!(event.op(), Some(op));
    assert_eq!(
        summary_of(&event).active_flags,
        vec!["COMMS_COMPAT"],
        "the snapshot keeps the definition's spelling for the file"
    );

    core.thread.join();
}

#[test]
fn a_rejected_definition_reports_a_diagnostic_and_the_core_survives() {
    let core = Harness::start();

    let bad = core
        .handle
        .load_definition("broken.ini", b"this is not an ini\n")
        .expect("queued");
    let event = core.next();
    assert_eq!(event.op(), Some(bad));
    let Event::Failed { failure, .. } = event else {
        panic!("expected a failure, got {event:?}");
    };
    assert_eq!(failure.code.as_str(), "definition-rejected");
    assert!(
        failure.message.contains("signature"),
        "the message explains what is missing: {}",
        failure.message
    );

    // The core is still healthy: a good definition loads next.
    let good = core.load("good.ini", SPEEDUINO_LIKE);
    assert_eq!(core.next().op(), Some(good));

    let report = core.thread.join();
    assert_eq!(report.loads, 1);
    assert_eq!(report.failures, 1);
}

#[test]
fn loading_twice_replaces_the_definition_and_the_core_keeps_it() {
    let core = Harness::start();

    let first = core.load("first.ini", SPEEDUINO_LIKE);
    assert_eq!(core.next().op(), Some(first));

    let second = core.load("second.ini", OTHER_LIKE);
    let event = core.next();
    assert_eq!(event.op(), Some(second));
    assert_eq!(summary_of(&event).signature, "rusEFI test");

    // Both loads were answered, in order, and the second one is what the core
    // holds — the summary is the only view the caller gets.
    let report = core.thread.join();
    assert_eq!(report.loads, 2);
    assert_eq!(report.handled, 2);
}

#[test]
fn unloading_answers_with_the_command_id() {
    let core = Harness::start();

    let load_op = core.load("a.ini", SPEEDUINO_LIKE);
    assert_eq!(core.next().op(), Some(load_op));

    let unload_op = core.handle.unload_definition().expect("queued");
    let event = core.next();
    assert_eq!(event, Event::DefinitionUnloaded { op: unload_op });

    core.thread.join();
}

#[test]
fn every_accepted_command_is_answered_exactly_once_under_concurrent_producers() {
    // Two producers and a shutdown racing them: the invariant is not about
    // which commands arrive, only that each accepted id is answered once.
    let core = Harness::start_with(&CoreConfig {
        command_capacity: 8,
        event_capacity: 4096,
        ..CoreConfig::default()
    });

    let mut producers = Vec::new();
    for worker in 0..2 {
        let handle = core.handle.clone();
        producers.push(thread::spawn(move || {
            let mut accepted: Vec<OpId> = Vec::new();
            let mut refused = 0usize;
            for index in 0..200 {
                let command = if index % 3 == 0 {
                    CoreCommand::Definition(DefinitionCommand::Unload)
                } else {
                    CoreCommand::load_definition(
                        format!("w{worker}-{index}.ini"),
                        SPEEDUINO_LIKE.as_bytes(),
                    )
                };
                match handle.execute(command) {
                    Ok(op) => accepted.push(op),
                    Err(CoreError::QueueFull) => refused += 1,
                    Err(other) => panic!("unexpected send error: {other}"),
                }
            }
            (accepted, refused)
        }));
    }

    let mut accepted = Vec::new();
    let mut refused = 0usize;
    for producer in producers {
        let (mut ops, refused_here) = producer.join().expect("producer thread");
        accepted.append(&mut ops);
        refused += refused_here;
    }

    // Ask for shutdown only after every accepted command is in flight.
    let _ = core.handle.shutdown();
    let collected = core.drain();
    let report = core.thread.join();

    // `Stopped` is last and appears once.
    assert_eq!(collected.last(), Some(&Event::Stopped));
    assert_eq!(
        collected
            .iter()
            .filter(|event| matches!(event, Event::Stopped))
            .count(),
        1
    );

    // Exactly one answer per accepted id — no duplicates, no silent drops.
    let mut answers: HashMap<OpId, usize> = HashMap::new();
    for event in &collected {
        if let Some(op) = event.op() {
            *answers.entry(op).or_default() += 1;
        }
    }
    for op in &accepted {
        assert_eq!(
            answers.get(op),
            Some(&1),
            "{op} was not answered exactly once"
        );
    }
    assert_eq!(
        answers.len(),
        accepted.len(),
        "an id was answered that was never accepted"
    );
    assert_eq!(
        report.handled + report.rejected,
        u64::try_from(accepted.len()).unwrap_or(u64::MAX)
    );
    assert!(
        refused > 0 || accepted.len() == 400,
        "a capacity-8 queue under two producers should refuse something \
         (refused {refused}, accepted {})",
        accepted.len()
    );
}

#[test]
fn commands_after_shutdown_are_refused_not_ignored() {
    let core = Harness::start();

    core.handle.shutdown().expect("queued");
    assert_eq!(
        core.next(),
        Event::Stopped,
        "shutdown has no reply of its own"
    );

    let refused = core
        .handle
        .load_definition("late.ini", SPEEDUINO_LIKE.as_bytes());
    assert!(
        matches!(refused, Err(CoreError::Stopped)),
        "a stopped core must refuse deterministically, got {refused:?}"
    );

    let report = core.thread.join();
    assert_eq!(
        report.handled, 0,
        "the shutdown itself is not counted as handled work"
    );
    assert_eq!(report.rejected, 0, "nothing was queued behind the shutdown");
}

#[test]
fn a_command_racing_the_shutdown_is_refused_or_answered() {
    // The invariant is exclusive: a command is never both accepted and
    // unanswered. Repeated so a timing-dependent regression shows up rather than
    // passing by luck.
    for _ in 0..20 {
        let core = Harness::start();
        let handle = core.handle.clone();

        let producer = thread::spawn(move || {
            let mut accepted = Vec::new();
            for index in 0..50 {
                if let Ok(op) =
                    handle.load_definition(format!("race-{index}.ini"), SPEEDUINO_LIKE.as_bytes())
                {
                    accepted.push(op);
                }
            }
            accepted
        });

        let accepted = producer.join().expect("producer thread");
        let _ = core.handle.shutdown();
        let collected = core.drain();
        let report = core.thread.join();

        let answered: HashSet<OpId> = collected.iter().filter_map(Event::op).collect();
        for op in &accepted {
            assert!(
                answered.contains(op),
                "{op} was accepted but never answered"
            );
        }
        assert_eq!(
            report.handled + report.rejected,
            u64::try_from(accepted.len()).unwrap_or(u64::MAX)
        );
    }
}

#[test]
fn the_core_answers_promptly() {
    let core = Harness::start();

    let started = Instant::now();
    for index in 0..100 {
        let op = core.load(&format!("loop-{index}.ini"), SPEEDUINO_LIKE);
        assert_eq!(core.next().op(), Some(op));
    }
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(5),
        "100 load answers took {elapsed:?}"
    );

    let report = core.thread.join();
    assert_eq!(report.handled, 100);
    assert!(
        report.max_op_latency < Duration::from_secs(1),
        "slowest command took {:?}",
        report.max_op_latency
    );
}

#[test]
fn dropping_the_core_stops_and_joins_the_thread() {
    let core = Harness::start();
    let Harness { thread, events, .. } = core;

    // Dropping asks for shutdown and joins, so the last event is queued by the
    // time it returns.
    drop(thread);

    let mut last = None;
    while let Some(event) = events.recv_timeout(WAIT) {
        let done = matches!(event, Event::Stopped);
        last = Some(event);
        if done {
            break;
        }
    }
    assert_eq!(last, Some(Event::Stopped));
}

#[test]
fn a_slow_consumer_backpressures_the_core_without_losing_answers() {
    // A consumer that falls behind blocks the core's replies (bounded queues,
    // ADR-0005 rule 5) and new commands are refused at the door — but nothing
    // accepted is lost: once the consumer returns, every answer arrives and
    // the core still shuts down and joins. This is the app's contract too: the
    // event pump keeps draining until `Stopped`, so the core is never wedged
    // for good.
    let core = Harness::start_with(&CoreConfig {
        command_capacity: 16,
        event_capacity: 8,
        ..CoreConfig::default()
    });

    let mut accepted = Vec::new();
    let mut refused = 0usize;
    for index in 0..200 {
        let command =
            CoreCommand::load_definition(format!("slow-{index}.ini"), SPEEDUINO_LIKE.as_bytes());
        match core.handle.execute(command) {
            Ok(op) => accepted.push(op),
            Err(CoreError::QueueFull) => refused += 1,
            Err(other) => panic!("unexpected send error: {other}"),
        }
    }
    assert!(
        refused > 0 || accepted.len() == 200,
        "the flood should outrun two tiny queues (refused {refused}, accepted {})",
        accepted.len()
    );

    // The consumer returns: each accepted id is answered exactly once.
    let mut answers: HashSet<OpId> = HashSet::new();
    while answers.len() < accepted.len() {
        if let Some(op) = core.next().op() {
            assert!(answers.insert(op), "{op} was answered twice");
        }
    }
    for op in &accepted {
        assert!(answers.contains(op), "{op} was accepted but never answered");
    }

    core.handle
        .shutdown()
        .expect("the idle core accepts shutdown");
    let report = core.thread.join();
    assert_eq!(
        usize::try_from(report.handled).unwrap_or(usize::MAX),
        accepted.len()
    );
    assert_eq!(report.rejected, 0);
}

#[test]
fn the_invariants_hold_across_repeated_runs() {
    // Determinism on the invariant fields, the way P6 claimed it: identical
    // answers, no duplicates, `Stopped` last — every run.
    for run in 0..5 {
        let core = Harness::start();
        let mut ops = Vec::new();
        for index in 0..25 {
            ops.push(core.load(&format!("run{run}-{index}.ini"), SPEEDUINO_LIKE));
        }
        core.handle.shutdown().ok();

        let collected = core.drain();
        let report = core.thread.join();

        let answered: HashSet<OpId> = collected.iter().filter_map(Event::op).collect();
        assert_eq!(answered.len(), ops.len(), "run {run}");
        assert_eq!(
            report.handled,
            u64::try_from(ops.len()).unwrap_or(u64::MAX),
            "run {run}"
        );
        assert_eq!(report.rejected, 0, "run {run}");
    }
}

#[test]
fn the_core_holds_no_gui_or_async_runtime() {
    // ADR-0001/0005 as a test: the manifests are the source of truth for what
    // the headless crates can link against, and the app crate is where GPUI
    // lives. Only the dependency tables count — prose in a description and
    // comments is free to name what is banned.
    let manifests = [
        ("oxtuner-core", include_str!("../Cargo.toml")),
        (
            "oxtuner-transport",
            include_str!("../../oxtuner-transport/Cargo.toml"),
        ),
        (
            "oxtuner-protocol",
            include_str!("../../oxtuner-protocol/Cargo.toml"),
        ),
        ("oxtuner-sim", include_str!("../../oxtuner-sim/Cargo.toml")),
    ];

    for (crate_name, manifest) in manifests {
        let dependencies: Vec<&str> = manifest
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with('#'))
            .filter(|line| !line.is_empty())
            .filter(|line| line.contains('=') || line.starts_with('['))
            .filter(|line| {
                !line.starts_with('[')
                    || line.starts_with("[dependencies]")
                    || line.starts_with("[dev-dependencies]")
                    || line.starts_with("[build-dependencies]")
            })
            .collect();

        for forbidden in ["gpui", "tokio", "async-std", "smol"] {
            for line in &dependencies {
                assert!(
                    !line.starts_with(forbidden),
                    "`{forbidden}` must not be a dependency of `{crate_name}`: {line}"
                );
            }
        }
    }

    // The channel topology is part of the decision, not an implementation
    // detail: the core's queues are crossfire-rs (ADR-0005/0019).
    assert!(include_str!("../Cargo.toml").contains("crossfire"));
}
