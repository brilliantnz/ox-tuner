//! The links this application supplies to the core.
//!
//! `oxtuner-core` opens serial devices itself. The simulator is the app's: it
//! is handed over through [`oxtuner_core::CustomLink`], so the session layer
//! never depends on a demo transport (ADR-0022).

use oxtuner_core::{CustomLink, Definition, PortChoice, Transport};
use oxtuner_sim::{SimConfig, SimTransport};

/// The simulator, as the core sees it.
pub(crate) struct SimulatorLink {
    config: SimConfig,
}

impl CustomLink for SimulatorLink {
    fn label(&self) -> &str {
        "simulator"
    }

    fn open(&self, definition: &Definition) -> Result<Box<dyn Transport>, String> {
        SimTransport::new(definition, self.config.clone())
            .map(|sim| -> Box<dyn Transport> { Box::new(sim) })
            .map_err(|error| error.to_string())
    }
}

/// The simulator with its default configuration.
pub(crate) fn simulator() -> PortChoice {
    PortChoice::custom(SimulatorLink {
        config: SimConfig::default(),
    })
}
