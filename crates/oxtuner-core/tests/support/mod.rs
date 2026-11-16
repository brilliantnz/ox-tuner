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

use oxtuner_core::{CustomLink, Definition, PortChoice, ProbeSpec, Transport};
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

    fn open_for_probe(&self, probe: &ProbeSpec) -> Result<Box<dyn Transport>, String> {
        Ok(Box::new(SimProbe::new(*probe)))
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

    fn open_for_probe(&self, probe: &ProbeSpec) -> Result<Box<dyn Transport>, String> {
        Ok(Box::new(SimProbe::silent(*probe)))
    }
}

/// A simulator connection with a specific configuration.
pub(crate) fn simulator_with(config: SimConfig) -> PortChoice {
    PortChoice::custom(Simulator::new(config))
}
