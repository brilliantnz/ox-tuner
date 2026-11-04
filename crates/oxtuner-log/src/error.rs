use std::io;

/// Why the logging pipeline could not start or could not be reconfigured.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum LogError {
    /// The log directory could not be created.
    #[error("could not create the log directory: {0}")]
    Directory(#[source] io::Error),

    /// The run file could not be opened.
    #[error("could not open the log file: {0}")]
    File(#[source] io::Error),

    /// A subscriber was already installed, or the global subscriber was
    /// rejected.
    #[error("could not install the tracing subscriber: {0}")]
    Init(String),

    /// The reloadable filter refused the new value.
    #[error("could not reload the log filter: {0}")]
    Filter(String),
}
