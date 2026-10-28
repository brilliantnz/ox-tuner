//! The headless core for ox-tuner.
//!
//! One OS thread owns the application's core state — the open session (a
//! definition, or a project with its tune images) and the live ECU session — and
//! answers typed commands with typed events over bounded
//! [crossfire](https://crates.io/crates/crossfire) channels. A second thread,
//! started per connection, owns the transport and the protocol (ADR-0008). No
//! `gpui*` type crosses either boundary, and no async runtime is used (ADR-0001,
//! ADR-0005).
//!
//! ```text
//!            Command / OpId                    Event
//!   UI ───────────────────────▶ [core thread] ───────────────────────▶ UI
//!                                   │ owns the
//!                          Definition / Project
//!                                   │ IoRequest / IoReply (SPSC)
//!                                   ▼
//!                             [I/O thread] ── Transport ──▶ ECU
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
//! The write path and guardrails S2–S10 (ADR-0010/0011, M7) and the datalog
//! ring buffer (ADR-0014, M8) belong to this crate — not to the UI. The live
//! read-only session (connect, signature verdict, page reads, polling, trace,
//! reconnect) is implemented; what it claims, it does.
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
mod io_thread;
mod menu;
mod message;
mod realtime;
mod service;

pub use handle::{BlockingEventStream, CoreHandle, EventStream};
pub use menu::{MenuDetail, MenuNode, MenuNodeKind};
pub use message::{
    AxisEdit, AxisKind, AxisSummary, CellEdit, Command, ConnectionSummary, DefinitionSummary,
    DialogFieldKind, DialogFieldSummary, DialogSummary, Event, Failure, FailureCode, OpId,
    PortChoice, PortSummary, ProjectSummary, ReadAllSummary, RejectReason, SaveSummary,
    SignatureVerdict, TableSummary,
};
pub use realtime::{LatestFrame, LiveValue, RealtimeDecoder, RealtimeFrame};
pub use service::{CoreConfig, CoreError, CoreParts, CoreReport, CoreThread, start};

/// Re-exported so callers can build [`Command::LoadDefinition`] without adding
/// a dependency on `oxtuner-ini` themselves.
pub use oxtuner_ini::SettingsContext;

/// Re-exported for the table grid's per-cell write-state decoration.
pub use oxtuner_tune::CellState;

/// Re-exported so a caller can pick the connection's framing without taking a
/// dependency on `oxtuner-protocol`.
pub use oxtuner_protocol::LinkMode;

/// Re-exported so a simulator connection can be configured — a signature
/// override or fault rules — without a second dependency on `oxtuner-sim`.
pub use oxtuner_sim::{Fault, FaultRule, SimConfig, SimOp};
