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
    Init(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// The reloadable filter refused the new value.
    #[error("could not reload the log filter: {0}")]
    Filter(#[source] Box<dyn std::error::Error + Send + Sync>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrapped_cause_stays_in_the_chain() {
        let init = LogError::Init(Box::new(io::Error::other("subscriber already installed")));
        let filter = LogError::Filter(Box::new(io::Error::other("bad directive")));

        for error in [init, filter] {
            assert!(
                std::error::Error::source(&error).is_some(),
                "the cause is lost: {error}"
            );
        }
    }
}
