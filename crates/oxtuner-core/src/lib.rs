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
//! - **Answered exactly once while the UI is reading.** [`CoreHandle::send`]
//!   returns the [`OpId`] the reply will carry; every accepted command produces
//!   exactly one `DefinitionLoaded` / `ProjectOpened` / `TuneSaved` /
//!   `DefinitionUnloaded` / `Failed` / `Rejected` event. A refused command
//!   ([`CoreError::QueueFull`]) never enters the core and receives no event —
//!   its id is simply unused.
//! - **Bounded and non-blocking for the UI.** Sending never blocks; a full
//!   command queue is reported, not waited on.
//! - **Realtime never enters the event queue.** The poller publishes
//!   [`LatestFrame`] and the chart history, then sends a coalesced
//!   [`FrameWatch`] wake, so a stalled UI cannot hold up acquisition, logging
//!   or reconnect (ADR-0021).
//! - **Bounded shutdown.** [`CoreThread::join`] (or dropping it) raises a stop
//!   flag, queues `Shutdown`, answers whatever is still queued with `Rejected`,
//!   and emits `Stopped`. Once quitting has started, an event that does not fit
//!   the queue is dropped and counted ([`CoreReport::dropped_events`]) instead
//!   of parking the core, so a consumer that stopped reading cannot keep the
//!   thread alive (ADR-0023). A core that is stuck inside a command is
//!   abandoned with an error log rather than hanging the caller.
//!
//! # Scope
//!
//! The write path and guardrails S2–S10 (ADR-0010/0011, M7), the bounded chart
//! history (ADR-0014, M8) and the CSV logger are implemented in this crate. The
//! M9 surface data belongs in the shared `history` module; no UI work belongs
//! here.
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
mod history;
mod io_thread;
mod journal;
mod link;
mod menu;
mod message;
mod realtime;
mod safety;
mod service;
mod snapshot;

pub use handle::{BlockingEventStream, CoreHandle, EventStream};
pub use history::{
    ChannelInfo, ChartHistory, HistoryStats, NearestSample, Sample, SeriesPoints, TimeWindow,
    decimate, nice_ticks, padded_range,
};
pub use link::{CustomLink, PortChoice};
pub use menu::{MenuDetail, MenuNode, MenuNodeKind};
pub use message::{
    AxisEdit, AxisKind, AxisSummary, BurnSummary, CellEdit, Command, ConnectionSummary, CurveAxes,
    DefinitionSummary, DialogFieldKind, DialogFieldSummary, DialogSummary, Event, Failure,
    FailureCode, ImportSummary, JournalEntry, OpId, PortSummary, ProjectSummary, ReadAllSummary,
    RejectReason, RevertSummary, SaveSummary, SignatureVerdict, TableSummary, WriteSummary,
};
pub use oxtuner_datalog::{LogColumn, LogSnapshot, LogStatus, LogSummary};
pub use realtime::{
    BlockingFrameWatch, FrameWatch, LatestFrame, LiveValue, RealtimeDecoder, RealtimeFrame,
};
pub use service::{CoreConfig, CoreError, CoreParts, CoreReport, CoreThread, start};

/// Re-exported so callers can build [`Command::LoadDefinition`] without adding
/// a dependency on `oxtuner-ini` themselves — and so a [`CustomLink`]
/// implementation can name the definition it is handed.
pub use oxtuner_ini::{Definition, SettingsContext};

/// Re-exported for the table grid's per-cell write-state decoration and the
/// write dialogs' pending counts.
pub use oxtuner_tune::{CellState, TuneStatus};

/// Re-exported so a caller can pick the connection's framing without taking a
/// dependency on `oxtuner-protocol`.
pub use oxtuner_protocol::LinkMode;

/// Re-exported so a [`CustomLink`] implementation can name the transport it
/// returns.
pub use oxtuner_transport::Transport;
