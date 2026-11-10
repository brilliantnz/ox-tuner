//! Realtime session logging: frames in, a CSV file out.
//!
//! The logger runs on its own thread behind a bounded channel (ADR-0014). The
//! poller never waits for a disk write: when the channel is full the frame is
//! dropped, the drop is counted and [`LogStatus::dropped`] surfaces it. A clean
//! [`Logger::stop`] flushes the buffer and joins the writer.
//!
//! ```text
//! poller ──try_send──▶ bounded channel ──▶ logger thread ──▶ BufWriter ──▶ .csv
//!    │                                          │
//!    └── drop counter ──▶ LogStatus ◀── frames / bytes
//! ```
//!
//! The header is the definition's channel order, with a leading `Time` column
//! in seconds since the session's first frame. Values are already in user units
//! when they reach this crate: the core decoded them (ADR-0006).

#![forbid(unsafe_code)]

mod csv;
mod logger;

pub use csv::LogColumn;
pub use logger::{LogStatus, Logger, SessionLogReport, SessionLogSnapshot};

#[cfg(test)]
mod tests;
