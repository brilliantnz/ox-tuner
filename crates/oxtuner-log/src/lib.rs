//! Process-wide logging for ox-tuner.
//!
//! One subscriber serves every crate. It owns the console, the per-run file,
//! the bounded in-memory ring the Diagnostics view reads, and the panic hook.
//! Libraries keep calling `tracing` macros directly; only the binary installs
//! a subscriber, and it does so through this crate.
//!
//! ```text
//! tracing macros ──▶ filter ──▶ console
//!                        ├────▶ per-run file
//!                        └────▶ LogStore ──▶ Diagnostics view
//! ```
//!
//! The profile chooses defaults, not code paths. A debug build writes to the
//! console and leaves the filesystem alone unless the caller asks for a file;
//! a release build writes a per-run file and uses the console only when stderr
//! is a terminal. The level can be raised while the process runs through
//! [`LogHandle::set_verbose`], unless `RUST_LOG` has set the filter.

#![forbid(unsafe_code)]

mod config;
mod error;
mod file;
mod filter;
mod init;
mod panic;
mod record;
mod store;

pub use config::{FilePolicy, LogConfig, Profile, Verbosity};
pub use error::LogError;
pub use init::{LogControl, LogHandle, init};
pub use record::{LogBufferSnapshot, LogRecord};
pub use store::LogStore;
