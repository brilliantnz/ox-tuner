//! Shared test wiring: the simulator, supplied the way the application does.
//!
//! `oxtuner-core` no longer names a simulator (ADR-0022), so every test that
//! wants one goes through the same [`CustomLink`] seam the app uses. Nothing
//! here is production code; the core's own transport path is what the tests
//! exercise.

#![expect(
    dead_code,
    reason = "not every integration test binary uses every helper"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use oxtuner_core::{
    BlockingEventStream, CustomLink, Definition, Event, PortChoice, ProbeQuery, Transport,
};
use oxtuner_sim::{SimConfig, SimProbe, SimProbeSet, SimTransport};

/// The simulator as a link implementation.
pub(crate) struct Simulator {
    config: SimConfig,
}

impl Simulator {
    /// A simulator with `config`.
    pub(crate) fn new(config: SimConfig) -> Self {
        Self { config }
    }
}

impl CustomLink for Simulator {
    fn label(&self) -> &'static str {
        "simulator"
    }

    fn open(&self, definition: &Definition) -> Result<Box<dyn Transport>, String> {
        SimTransport::new(definition, self.config.clone())
            .map(|sim| -> Box<dyn Transport> { Box::new(sim) })
            .map_err(|error| error.to_string())
    }

    fn open_for_probe(&self, queries: &[ProbeQuery]) -> Result<Box<dyn Transport>, String> {
        Ok(Box::new(SimProbeSet::new(
            queries
                .iter()
                .map(|query| query.probe.spec().with_link(query.framing)),
        )))
    }
}

/// A simulator connection with the default configuration.
pub(crate) fn simulator() -> PortChoice {
    PortChoice::custom(Simulator::new(SimConfig::default()))
}

/// A link that opens and then never answers, like a port with nothing attached.
pub(crate) fn silent() -> PortChoice {
    PortChoice::custom(Silent)
}

/// A port that opens and answers nothing, like one with no ECU attached.
struct Silent;

/// A probe link that counts its opens and leaves the first `silent` queries
/// unanswered, like a port where the first family tried is not the one there.
pub(crate) fn counting_probe(silent: usize) -> (PortChoice, Arc<AtomicUsize>) {
    let opens = Arc::new(AtomicUsize::new(0));

    (
        PortChoice::custom(Counting {
            opens: Arc::clone(&opens),
            silent,
        }),
        opens,
    )
}

struct Counting {
    opens: Arc<AtomicUsize>,
    silent: usize,
}

impl CustomLink for Counting {
    fn label(&self) -> &'static str {
        "counting"
    }

    fn open(&self, _definition: &Definition) -> Result<Box<dyn Transport>, String> {
        Err("the counting link only answers probes".to_owned())
    }

    fn open_for_probe(&self, queries: &[ProbeQuery]) -> Result<Box<dyn Transport>, String> {
        self.opens.fetch_add(1, Ordering::Relaxed);

        let probes = queries.iter().enumerate().map(|(index, query)| {
            let spec = query.probe.spec().with_link(query.framing);
            if index < self.silent {
                SimProbe::silent(spec)
            } else {
                SimProbe::new(spec)
            }
        });

        Ok(Box::new(SimProbeSet::from_probes(probes)))
    }
}

impl CustomLink for Silent {
    fn label(&self) -> &'static str {
        "silent"
    }

    fn open(&self, _definition: &Definition) -> Result<Box<dyn Transport>, String> {
        Ok(Box::new(SimProbe::silent(oxtuner_protocol::SPEEDUINO)))
    }

    fn open_for_probe(&self, queries: &[ProbeQuery]) -> Result<Box<dyn Transport>, String> {
        Ok(Box::new(SimProbeSet::silent(
            queries
                .iter()
                .map(|query| query.probe.spec().with_link(query.framing)),
        )))
    }
}

/// A port whose probe answers with `text`, like an adapter that echoes, spams
/// debug lines or speaks NMEA over the query.
pub(crate) fn answering_probe(text: &'static str) -> PortChoice {
    PortChoice::custom(Answering(text))
}

struct Answering(&'static str);

impl CustomLink for Answering {
    fn label(&self) -> &'static str {
        // One label for every payload: nothing compares two of these today,
        // and `PortChoice::eq` matches custom links by label.
        "answering"
    }

    fn open(&self, _definition: &Definition) -> Result<Box<dyn Transport>, String> {
        // Probe-only: the answer is what this link exists for, so a session
        // opened on it stays silent, the same as [`Silent`].
        Ok(Box::new(SimProbe::silent(oxtuner_protocol::SPEEDUINO)))
    }

    fn open_for_probe(&self, queries: &[ProbeQuery]) -> Result<Box<dyn Transport>, String> {
        Ok(Box::new(SimProbeSet::with_signature(
            queries
                .iter()
                .map(|query| query.probe.spec().with_link(query.framing)),
            self.0,
        )))
    }
}

/// A simulator connection with a specific configuration.
pub(crate) fn simulator_with(config: SimConfig) -> PortChoice {
    PortChoice::custom(Simulator::new(config))
}

/// Receives the next event that is not a port-list publication.
///
/// The core pushes its port snapshot on its own, including the first sample,
/// so a harness waiting for a command's answer steps over those instead of
/// tripping on them. Tests that assert on the port list itself wait for the
/// publication explicitly and do not use this.
pub(crate) fn recv_non_port(events: &BlockingEventStream, wait: Duration) -> Option<Event> {
    let deadline = Instant::now() + wait;

    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let event = events.recv_timeout(left)?;

        if !matches!(
            event,
            Event::PortsChanged { .. } | Event::PortsListed { .. }
        ) {
            return Some(event);
        }
    }
}
