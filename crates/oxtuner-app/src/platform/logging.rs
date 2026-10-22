//! Logging bootstrap.
//!
//! One rolling file in the platform's log directory, plus stderr while running
//! from a terminal. The guard returned here must live for the whole process;
//! dropping it stops the non-blocking writer from flushing the last records.

use std::fs;
use std::io::IsTerminal as _;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt as _, util::SubscriberInitExt as _};

use super::paths::AppPaths;

/// Installs the process-wide tracing subscriber.
///
/// Returns `None` when the log directory cannot be created; the app still runs,
/// it just has no file log. An invalid level string falls back to `info`
/// instead of refusing to start.
pub fn init(paths: &AppPaths, level: &str) -> Option<WorkerGuard> {
    let directory = paths.log_dir();
    if let Err(error) = fs::create_dir_all(directory) {
        eprintln!(
            "ox-tuner: cannot create log directory {}: {error}",
            directory.display()
        );
        return None;
    }

    let (writer, guard) =
        tracing_appender::non_blocking(tracing_appender::rolling::daily(directory, "oxtuner.log"));

    let filter = EnvFilter::try_new(level).unwrap_or_else(|error| {
        eprintln!("ox-tuner: invalid log level {level:?} ({error}), using info");
        EnvFilter::new("info")
    });

    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(std::io::stderr().is_terminal())
                .with_writer(std::io::stderr),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(writer),
        )
        .try_init();

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        log_dir = %directory.display(),
        "ox-tuner starting"
    );

    Some(guard)
}
