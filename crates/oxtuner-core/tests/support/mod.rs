//! Shared test wiring: the simulator, supplied the way the application does.
//!
//! `oxtuner-core` no longer names a simulator (ADR-0022), so every test that
//! wants one goes through the same [`CustomLink`] seam the app uses. Nothing
//! here is production code; the core's own transport path is what the tests
//! exercise.

#![allow(dead_code)]

use oxtuner_core::{CustomLink, Definition, PortChoice, Transport};
use oxtuner_sim::{SimConfig, SimTransport};

/// The simulator as a link implementation.
pub struct Simulator {
    config: SimConfig,
}

impl Simulator {
    /// A simulator with `config`.
    pub fn new(config: SimConfig) -> Self {
        Self { config }
    }
}

impl CustomLink for Simulator {
    fn label(&self) -> &str {
        "simulator"
    }

    fn open(&self, definition: &Definition) -> Result<Box<dyn Transport>, String> {
        SimTransport::new(definition, self.config.clone())
            .map(|sim| -> Box<dyn Transport> { Box::new(sim) })
            .map_err(|error| error.to_string())
    }
}

/// A simulator connection with the default configuration.
pub fn simulator() -> PortChoice {
    PortChoice::custom(Simulator::new(SimConfig::default()))
}

/// A simulator connection with a specific configuration.
pub fn simulator_with(config: SimConfig) -> PortChoice {
    PortChoice::custom(Simulator::new(config))
}
