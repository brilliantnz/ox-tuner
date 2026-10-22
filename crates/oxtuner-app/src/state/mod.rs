//! Persisted application state.
//!
//! The session document (`state.json`) and the JSON store both the settings
//! and the session are written through. Everything here is I/O and plain
//! data; views read it through GPUI globals, never directly from disk.

pub mod session;
pub(crate) mod store;
