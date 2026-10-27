//! The headless core for ox-tuner.
//!
//! One OS thread owns the application's core state — today the open session
//! (a definition, or a project with its tune images), next the ECU session —
//! and answers typed commands with typed events over bounded
//! [crossfire](https://crates.io/crates/crossfire) channels. No `gpui*` type
//! crosses this boundary, and no async runtime is used (ADR-0001, ADR-0005).
//!
//! ```text
//!            Command / OpId                    Event
//!   UI ───────────────────────▶ [core thread] ───────────────────────▶ UI
//!                                   owns the
//!                              Definition / Project
//! ```
//!
//! # Guarantees
//!
//! - **One owner.** The definition and the tune are moved into the core thread
//!   and dropped there. Callers receive a [`DefinitionSummary`] or a
//!   [`ProjectSummary`], never the data.
//! - **Answered exactly once.** [`CoreHandle::send`] returns the [`OpId`] the
//!   reply will carry; every accepted command produces exactly one
//!   `DefinitionLoaded` / `ProjectOpened` / `TuneSaved` / `DefinitionUnloaded` /
//!   `Failed` / `Rejected` event. A refused command ([`CoreError::QueueFull`])
//!   never enters the core and receives no event — its id is simply unused.
//! - **Bounded and non-blocking for the UI.** Sending never blocks; a full
//!   command queue is reported, not waited on.
//! - **Deterministic shutdown.** [`CoreThread::join`] (or dropping it) queues
//!   `Shutdown`, answers whatever is still queued with `Rejected`, emits
//!   `Stopped`, and joins.
//!
//! # What lands here next
//!
//! The transport (ADR-0008, M5), the ECU-side tune images and the write path
//! (ADR-0010/0011, M7), the realtime poller (ADR-0014, M8) and guardrails
//! S1–S10 (ADR-0011, M7) all belong to this crate — not to the UI. Nothing
//! above is stubbed for them: the core is complete for the lifecycle it claims.
//!
//! # Example
//!
//! ```
//! use oxtuner_core::{CoreConfig, Event, start};
//! use std::time::Duration;
//!
//! let core = start(CoreConfig::default()).expect("the core starts");
//! let events = core.events.into_blocking();
//!
//! let text = "[MegaTune]\nsignature = \"speeduino 202501\"\nqueryCommand = \"Q\"\n";
//! let op = core
//!     .handle
//!     .load_definition("speeduino.ini", text.as_bytes())
//!     .expect("queued");
//!
//! let event = events.recv_timeout(Duration::from_secs(5)).expect("an answer");
//! let Event::DefinitionLoaded { op: answered, summary } = event else {
//!     panic!("expected a load");
//! };
//! assert_eq!(answered, op);
//! assert_eq!(summary.signature, "speeduino 202501");
//!
//! core.thread.join();
//! ```

mod handle;
mod menu;
mod message;
mod service;

pub use handle::{BlockingEventStream, CoreHandle, EventStream};
pub use menu::{MenuDetail, MenuNode, MenuNodeKind};
pub use message::{
    AxisEdit, AxisKind, AxisSummary, CellEdit, Command, DefinitionSummary, DialogFieldSummary,
    DialogSummary, Event, Failure, FailureCode, OpId, ProjectSummary, RejectReason, SaveSummary,
    TableSummary,
};
pub use service::{CoreConfig, CoreError, CoreParts, CoreReport, CoreThread, start};

/// Re-exported so callers can build [`Command::LoadDefinition`] without adding
/// a dependency on `oxtuner-ini` themselves.
pub use oxtuner_ini::SettingsContext;

/// Re-exported for the table grid's per-cell write-state decoration.
pub use oxtuner_tune::CellState;
