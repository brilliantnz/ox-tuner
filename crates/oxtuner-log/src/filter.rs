use tracing_subscriber::EnvFilter;

use crate::config::{Profile, Verbosity};

/// The filter chosen for this process.
pub(crate) struct FilterSetup {
    pub filter: EnvFilter,
    pub env_override: bool,
    pub warning: Option<String>,
}

/// Uses `RUST_LOG` when it is valid; otherwise builds the profile default.
///
/// `RUST_LOG` wins over the persisted verbose setting. An invalid value falls
/// back to the default instead of refusing to start, and the warning is logged
/// once the subscriber is installed.
pub(crate) fn setup(profile: Profile, verbosity: Verbosity) -> FilterSetup {
    match EnvFilter::try_from_default_env() {
        Ok(filter) => FilterSetup {
            filter,
            env_override: true,
            warning: None,
        },
        Err(error) => FilterSetup {
            filter: default_filter(profile, verbosity),
            env_override: false,
            warning: std::env::var_os("RUST_LOG")
                .is_some()
                .then(|| format!("invalid RUST_LOG ({error}), using the default filter")),
        },
    }
}

/// The workspace at the profile level, everything else at `warn`.
///
/// The `oxtuner_` target prefix covers every workspace crate without naming
/// them one by one. A dependency that happens to start with the same prefix
/// would be treated as workspace code; the workspace list is small enough that
/// this is acceptable, and it keeps the default from rotting when a crate is
/// added.
pub(crate) fn default_filter(profile: Profile, verbosity: Verbosity) -> EnvFilter {
    let level = if verbosity.is_verbose() || profile == Profile::Debug {
        "debug"
    } else {
        "info"
    };

    EnvFilter::new(format!("warn,oxtuner_={level}"))
}
