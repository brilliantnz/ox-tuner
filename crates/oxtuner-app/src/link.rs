//! The links this application supplies to the core.
//!
//! `oxtuner-core` opens serial devices itself. The simulator is the app's: it
//! is handed over through [`oxtuner_core::CustomLink`], so the session layer
//! never depends on a demo transport (ADR-0022).

use oxtuner_core::{CustomLink, Definition, PortChoice, ProbeQuery, Transport};
use oxtuner_sim::{SimConfig, SimProbeSet, SimTransport};

/// The simulator, as the core sees it.
pub(crate) struct SimulatorLink {
    config: SimConfig,
}

impl CustomLink for SimulatorLink {
    fn label(&self) -> &'static str {
        "simulator"
    }

    fn open(&self, definition: &Definition) -> Result<Box<dyn Transport>, String> {
        SimTransport::new(definition, self.config.clone())
            .map(|sim| -> Box<dyn Transport> { Box::new(sim) })
            .map_err(|error| error.to_string())
    }

    fn open_for_probe(&self, queries: &[ProbeQuery]) -> Result<Box<dyn Transport>, String> {
        // The simulated ECU answers every family the scan asks, in the framing
        // it asked, so one open port serves the whole group.
        Ok(Box::new(SimProbeSet::new(
            queries
                .iter()
                .map(|query| query.probe.spec().with_link(query.framing)),
        )))
    }
}

/// The simulator with its default configuration.
pub(crate) fn simulator() -> PortChoice {
    PortChoice::custom(SimulatorLink {
        config: SimConfig::default(),
    })
}
