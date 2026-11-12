//! Logging bootstrap.
//!
//! The pipeline itself lives in `oxtuner-log`. This module supplies the app's
//! platform log directory, keeps the handle alive for the process, and gives
//! the Settings page a way to change the filter.
//!
//! The handle is stored in a GPUI global so the writer guard outlives the
//! window and the filter control is reachable without threading a reference
//! through every view.

use std::path::Path;

use gpui_kit::{App, Global};
use oxtuner_log::{FilePolicy, LogConfig, LogHandle, LogStore};

/// The installed pipeline, kept for the whole process.
#[derive(Debug)]
pub struct Logging {
    handle: LogHandle,
}

impl Global for Logging {}

/// Whether the logging pipeline also writes a run file.
#[expect(clippy::exhaustive_enums, reason = "the file policy is a closed set")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOutput {
    /// Write a file automatically when the build profile asks for one.
    Auto,
    /// Write a run file even when the profile would not.
    On,
}

/// Installs the process-wide subscriber.
///
/// Returns `None` when the log directory cannot be created. The app still
/// starts; it just has no file and no in-memory tail. The caller must keep the
/// handle alive or hand it to [`install`]. The filter starts at the profile
/// default; the saved verbose preference is applied after it loads.
pub fn init(directory: &Path, file: FileOutput) -> Option<LogHandle> {
    let policy = match file {
        FileOutput::Auto => FilePolicy::Auto,
        FileOutput::On => FilePolicy::On,
    };
    let config = LogConfig::new(directory).with_file(policy);

    match oxtuner_log::init(&config) {
        Ok(handle) => Some(handle),
        Err(error) => {
            #[expect(
                clippy::print_stderr,
                reason = "the logging pipeline failed to start, so tracing cannot report it"
            )]
            {
                eprintln!("ox-tuner: logging is unavailable: {error}");
            }
            None
        }
    }
}

/// Stores the handle in the GPUI global for the rest of the process.
pub fn install(cx: &mut App, handle: LogHandle) {
    cx.set_global(Logging { handle });
}

/// The installed handle, when logging started.
pub fn handle(cx: &App) -> Option<&LogHandle> {
    cx.try_global::<Logging>().map(|logging| &logging.handle)
}

/// The bounded ring, cloned for a view that wants to read it on its tick.
pub fn store(cx: &App) -> Option<LogStore> {
    handle(cx).map(LogHandle::store)
}

/// Applies the persisted verbose preference to the running filter.
///
/// `RUST_LOG` keeps the final say; in that case this is a no-op inside the
/// logger and only the saved preference changes.
pub fn set_verbose(cx: &App, verbose: bool) {
    if let Some(handle) = handle(cx)
        && let Err(error) = handle.set_verbose(verbose)
    {
        tracing::warn!(%error, "could not change the log filter");
    }
}
