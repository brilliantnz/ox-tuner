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

use std::time::Duration;

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

/// A port that accepts writes and answers nothing.
struct Silent;

impl CustomLink for Silent {
    fn label(&self) -> &'static str {
        "silent"
    }

    fn open(&self, _definition: &Definition) -> Result<Box<dyn Transport>, String> {
        Ok(Box::new(SilentTransport))
    }

    fn open_for_probe(&self, _probe: &ProbeSpec) -> Result<Box<dyn Transport>, String> {
        Ok(Box::new(SilentTransport))
    }
}

/// A transport whose input never arrives.
struct SilentTransport;

impl Transport for SilentTransport {
    fn open(&mut self) -> Result<(), oxtuner_transport::TransportError> {
        Ok(())
    }

    fn write_all(&mut self, _bytes: &[u8]) -> Result<(), oxtuner_transport::TransportError> {
        Ok(())
    }

    fn read_exact_timeout(
        &mut self,
        buf: &mut [u8],
        _timeout: Duration,
    ) -> Result<(), oxtuner_transport::TransportError> {
        Err(oxtuner_transport::TransportError::Timeout {
            got: 0,
            expected: buf.len(),
        })
    }

    fn read_until(
        &mut self,
        _sink: &mut Vec<u8>,
        _max: usize,
        _terminator: u8,
        _quiet: Duration,
        _timeout: Duration,
    ) -> Result<(), oxtuner_transport::TransportError> {
        Err(oxtuner_transport::TransportError::Timeout {
            got: 0,
            expected: 1,
        })
    }

    fn read_available(
        &mut self,
        _buf: &mut [u8],
    ) -> Result<usize, oxtuner_transport::TransportError> {
        Ok(0)
    }

    fn flush_input(&mut self) {}

    fn close(&mut self) {}
}

/// A simulator connection with a specific configuration.
pub(crate) fn simulator_with(config: SimConfig) -> PortChoice {
    PortChoice::custom(Simulator::new(config))
}
