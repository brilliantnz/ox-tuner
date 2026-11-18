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

use std::time::{Duration, Instant};

use oxtuner_core::{
    BlockingEventStream, CustomLink, Definition, Event, FirmwareProbe, PortChoice, Transport,
};
use oxtuner_sim::{SimConfig, SimProbe, SimTransport};

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

    fn open_for_probe(
        &self,
        probe: &FirmwareProbe,
        framing: oxtuner_core::LinkMode,
    ) -> Result<Box<dyn Transport>, String> {
        Ok(Box::new(SimProbe::new(probe.spec().with_link(framing))))
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

impl CustomLink for Silent {
    fn label(&self) -> &'static str {
        "silent"
    }

    fn open(&self, _definition: &Definition) -> Result<Box<dyn Transport>, String> {
        Ok(Box::new(SimProbe::silent(oxtuner_protocol::SPEEDUINO)))
    }

    fn open_for_probe(
        &self,
        probe: &FirmwareProbe,
        framing: oxtuner_core::LinkMode,
    ) -> Result<Box<dyn Transport>, String> {
        Ok(Box::new(SimProbe::silent(probe.spec().with_link(framing))))
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
        "answering"
    }

    fn open(&self, _definition: &Definition) -> Result<Box<dyn Transport>, String> {
        Ok(Box::new(SimProbe::silent(oxtuner_protocol::SPEEDUINO)))
    }

    fn open_for_probe(
        &self,
        probe: &FirmwareProbe,
        framing: oxtuner_core::LinkMode,
    ) -> Result<Box<dyn Transport>, String> {
        Ok(Box::new(SimProbe::with_signature(
            probe.spec().with_link(framing),
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
