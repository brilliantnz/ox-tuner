//! A virtual ECU built from a real definition (ADR-0012).
//!
//! The simulator is an [`oxtuner_transport::Transport`]: it receives the same command bytes the
//! protocol would send to hardware and answers them from the definition's own
//! message set. That is the point — the simulator exercises the production
//! protocol path, not a mock of it.
//!
//! ```text
//! Definition ──▶ SimTransport ── pages, flash, realtime, signature
//!                     ▲
//!          fault injection, scenario, seed
//! ```
//!
//! # What it models
//!
//! - **Memory** — a RAM image per page and a separate burned image. Reads see
//!   RAM; a burn copies one page to flash. Writes never touch flash.
//! - **Realtime** — a deterministic block of plausible engine values (RPM,
//!   MAP, TPS, coolant, intake, AFR, battery) at the definition's offsets and
//!   storage types, clamped to the definition's ranges.
//! - **Faults** — a reply can be dropped, delayed, truncated or corrupted; the
//!   link can die; a burn can be refused. Faults are armed per operation and
//!   fire a set number of times, so the recovery path is testable too.
//! - **Framing** — legacy and `msEnvelope_1.0` are both understood on the same
//!   transport; the first complete frame decides which one the host is using.
//!
//! # Determinism
//!
//! The same definition, seed and request sequence produce byte-identical
//! replies. No wall clock, no random source: the waveform is a function of the
//! request step, and the page fill is a function of the seed and page number.
//!
//! # Scenarios
//!
//! [`Scenario::parse`] reads a small line-based file so a test (or a developer)
//! can describe a session without writing code:
//!
//! ```text
//! # a bench regression
//! seed = 7
//! signature = speeduino broken
//! fault = read-page:truncate:32
//! fault = burn:drop x2
//! ```
//!
//! # Example
//!
//! ```
//! use oxtuner_ini::{SettingsContext, parse_str};
//! use oxtuner_sim::SimTransport;
//!
//! let definition = parse_str(
//!     "\
//! [MegaTune]
//! signature = \"speeduino 202501\"
//! queryCommand = \"Q\"
//!
//! [Constants]
//! nPages = 1
//! pageSize = 4
//! page = 1
//! pageReadCommand = \"p%2i%2o%2c\"
//! reqFuel = scalar, U08, 0, \"ms\", 0.1, 0, 0, 25.5, 1
//! ",
//!     &SettingsContext::new(),
//! )
//! .expect("the definition parses");
//!
//! let sim = SimTransport::new(&definition, Default::default()).expect("the definition has pages");
//! assert_eq!(sim.page_size(1), Some(4));
//! ```

#![forbid(unsafe_code)]

mod ecu;
mod faults;
mod memory;
mod probe;
mod queue;
mod realtime;
mod scenario;
mod transport;

pub use faults::{Fault, FaultRule, SimOp};
pub use memory::{Memory, PageMemory};
pub use oxtuner_protocol::LinkMode;
pub use probe::{SimProbe, SimProbeSet};
pub use scenario::{Scenario, ScenarioError};
pub use transport::{SimConfig, SimTransport, Transcript};

/// Why a simulator could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimError {
    /// A definition the simulator cannot represent.
    pub message: String,
}

impl std::fmt::Display for SimError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SimError {}
