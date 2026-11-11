use std::{
    fs,
    io::IsTerminal as _,
    path::{Path, PathBuf},
};

use time::OffsetDateTime;
use tracing_appender::non_blocking::{ErrorCounter, WorkerGuard};
use tracing_subscriber::{
    EnvFilter, fmt, layer::SubscriberExt as _, registry::Registry, reload,
    util::SubscriberInitExt as _,
};

use crate::{
    config::{LogConfig, Profile},
    error::LogError,
    file::{self, RunFile},
    filter::{self, FilterSetup},
    record::LogBufferSnapshot,
    store::{LogStore, StoreLayer},
};

/// The installed logging pipeline.
///
/// Keep this value alive for the process. Dropping it drops the non-blocking
/// writer guard, which flushes the records already queued for the file.
pub struct LogHandle {
    store: LogStore,
    path: Option<PathBuf>,
    // The guard must outlive every log call. The underscore is the field's
    // only purpose: keeping it alive.
    _guard: Option<WorkerGuard>,
    file_dropped: Option<ErrorCounter>,
    control: LogControl,
}

impl std::fmt::Debug for LogHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LogHandle")
            .field("store", &self.store)
            .field("path", &self.path)
            .field("file_dropped", &self.file_dropped)
            .finish_non_exhaustive()
    }
}

impl LogHandle {
    /// A handle to the in-memory ring for a view or a test.
    pub fn store(&self) -> LogStore {
        self.store.clone()
    }

    /// The active run file, or `None` when this build writes no file.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Lines the non-blocking writer dropped because its queue was full.
    ///
    /// The file writer is lossy on purpose: a slow disk must not block the
    /// thread that logged. The count is zero when no file is written.
    pub fn file_dropped(&self) -> u64 {
        self.file_dropped.as_ref().map_or(0, |counter| {
            u64::try_from(counter.dropped_lines()).unwrap_or(u64::MAX)
        })
    }

    /// A cloneable control for changing the filter from a view.
    pub fn control(&self) -> LogControl {
        self.control.clone()
    }

    /// Raises or lowers the workspace level while the process runs.
    ///
    /// `RUST_LOG` has the final say; when it set the filter this is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`LogError::Filter`] when the reloadable filter rejects the new
    /// value.
    pub fn set_verbose(&self, verbose: bool) -> Result<(), LogError> {
        self.control.set_verbose(verbose)
    }

    /// A frozen copy of the ring, for one render.
    pub fn snapshot(&self) -> LogBufferSnapshot {
        self.store.snapshot()
    }
}

/// A cloneable handle for changing the filter without owning the writer.
#[derive(Clone)]
pub struct LogControl {
    reload: reload::Handle<EnvFilter, Registry>,
    profile: Profile,
    env_override: bool,
}

impl std::fmt::Debug for LogControl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LogControl")
            .field("profile", &self.profile)
            .field("env_override", &self.env_override)
            .finish_non_exhaustive()
    }
}

impl LogControl {
    /// Raises or lowers the workspace level while the process runs.
    ///
    /// `RUST_LOG` wins over the persisted setting, so this does nothing when
    /// the environment set the filter. The result is still `Ok(())` because
    /// the request is not an error, only overridden.
    ///
    /// # Errors
    ///
    /// Returns [`LogError::Filter`] when the reloadable filter rejects the new
    /// value.
    pub fn set_verbose(&self, verbose: bool) -> Result<(), LogError> {
        if self.env_override {
            return Ok(());
        }

        let next = filter::default_filter(self.profile, verbose);
        self.reload
            .reload(next)
            .map_err(|error| LogError::Filter(Box::new(error)))
    }

    /// Whether `RUST_LOG` supplied the filter and therefore owns the level.
    pub fn env_override(&self) -> bool {
        self.env_override
    }
}

/// Installs the process-wide subscriber and returns the handle it needs.
///
/// This is called once by the binary, before the first window. A second call
/// returns [`LogError::Init`] instead of installing a second subscriber.
///
/// # Errors
///
/// Returns [`LogError::Directory`] or [`LogError::File`] when the log paths
/// cannot be prepared, and [`LogError::Init`] when a subscriber is already
/// installed.
pub fn init(config: &LogConfig) -> Result<LogHandle, LogError> {
    fs::create_dir_all(config.directory()).map_err(LogError::Directory)?;

    let now = OffsetDateTime::now_utc();
    if let Err(error) = file::prune(config.directory(), now) {
        // The subscriber does not exist yet, so stderr is the only place this
        // can go. A failed prune must not stop the application from starting.
        #[expect(
            clippy::print_stderr,
            reason = "the logging subscriber is not installed yet, so tracing cannot report this"
        )]
        {
            eprintln!("ox-tuner: could not prune old log files: {error}");
        }
    }

    let file_enabled = config.file_enabled();
    let (file_sink, path, guard, file_dropped) = if file_enabled {
        let run = RunFile::create(config.directory(), now).map_err(LogError::File)?;
        let path = run.path().to_path_buf();
        let (writer, guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
            .lossy(true)
            .thread_name("log-writer")
            .finish(run);
        let file_dropped = writer.error_counter();

        (Some(writer), Some(path), Some(guard), Some(file_dropped))
    } else {
        (None, None, None, None)
    };

    let FilterSetup {
        filter,
        env_override,
        warning,
    } = filter::setup(config.profile(), config.verbose());
    let filter_text = filter.to_string();
    let (filter, reload) = reload::Layer::new(filter);
    let store = LogStore::new(config.ring_capacity());

    let terminal = std::io::stderr().is_terminal();
    let console = (config.profile() == Profile::Debug || terminal).then(|| {
        fmt::layer()
            .with_writer(std::io::stderr)
            .with_ansi(terminal)
            .with_level(true)
            .with_target(true)
            .with_thread_names(true)
    });
    let file_layer = file_sink.map(|sink| {
        fmt::layer()
            .with_writer(sink)
            .with_ansi(false)
            .with_level(true)
            .with_target(true)
            .with_thread_names(true)
    });

    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(console)
        .with(file_layer)
        .with(StoreLayer::new(store.clone()));

    // A dependency that still uses `log` goes through the same subscriber;
    // `try_init` installs the bridge when `tracing-subscriber` was built with
    // the `tracing-log` feature.
    subscriber
        .try_init()
        .map_err(|error| LogError::Init(Box::new(error)))?;

    crate::panic::install();

    if let Some(warning) = warning {
        tracing::warn!(%warning, "RUST_LOG was ignored");
    }

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        profile = ?config.profile(),
        filter = %filter_text,
        log_file = path.as_ref().map(|path| path.display().to_string()),
        "ox-tuner starting"
    );

    Ok(LogHandle {
        store,
        path,
        _guard: guard,
        file_dropped,
        control: LogControl {
            reload,
            profile: config.profile(),
            env_override,
        },
    })
}
