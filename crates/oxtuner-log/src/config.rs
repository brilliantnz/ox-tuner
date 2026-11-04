use std::path::{Path, PathBuf};

/// The build profile the process is running under.
///
/// The profile only selects defaults. Call sites are never gated on it, so a
/// shipped binary can still raise its own level at runtime.
#[expect(
    clippy::exhaustive_enums,
    reason = "a build is either debug or release; the set cannot grow"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// A development build: console by default, no file.
    Debug,
    /// A shipped build: a file by default, console only with a terminal.
    Release,
}

impl Profile {
    /// The profile this binary was compiled for.
    pub const fn current() -> Self {
        if cfg!(debug_assertions) {
            Self::Debug
        } else {
            Self::Release
        }
    }
}

/// When the file layer should write.
#[expect(
    clippy::exhaustive_enums,
    reason = "auto, on and off cover every file policy"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FilePolicy {
    /// A file in release builds, no file in debug builds.
    #[default]
    Auto,
    /// Always write a file, including a debug build.
    On,
    /// Never write a file, including a release build.
    Off,
}

/// Configuration for the process-wide logging pipeline.
///
/// Build it with [`LogConfig::new`] and the builder methods. The directory is
/// created when [`init`](crate::init) runs.
#[derive(Debug, Clone)]
pub struct LogConfig {
    directory: PathBuf,
    profile: Profile,
    file: FilePolicy,
    verbose: bool,
    ring_capacity: usize,
}

impl LogConfig {
    /// A configuration that writes into `directory`.
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
            profile: Profile::current(),
            file: FilePolicy::Auto,
            verbose: false,
            ring_capacity: 5_000,
        }
    }

    /// Selects a profile explicitly. The binary normally keeps
    /// [`Profile::current`], so this is mainly for tests and tools.
    #[must_use]
    pub fn with_profile(mut self, profile: Profile) -> Self {
        self.profile = profile;
        self
    }

    /// Overrides when a file is written.
    #[must_use]
    pub fn with_file(mut self, policy: FilePolicy) -> Self {
        self.file = policy;
        self
    }

    /// Starts with the verbose level, normally from the persisted setting.
    #[must_use]
    pub fn with_verbose(mut self, verbose: bool) -> Self {
        self.verbose = verbose;
        self
    }

    /// Changes how many records the in-memory ring keeps.
    ///
    /// The ring is a view buffer, not the file. A larger value costs memory
    /// per record; a smaller value drops older records sooner.
    #[must_use]
    pub fn with_ring_capacity(mut self, capacity: usize) -> Self {
        self.ring_capacity = capacity.max(1);
        self
    }

    pub(crate) fn directory(&self) -> &Path {
        &self.directory
    }

    pub(crate) fn profile(&self) -> Profile {
        self.profile
    }

    pub(crate) fn file_enabled(&self) -> bool {
        match self.file {
            FilePolicy::Auto => self.profile == Profile::Release,
            FilePolicy::On => true,
            FilePolicy::Off => false,
        }
    }

    pub(crate) fn verbose(&self) -> bool {
        self.verbose
    }

    pub(crate) fn ring_capacity(&self) -> usize {
        self.ring_capacity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_writes_a_file_only_in_release() {
        let debug = LogConfig::new("/tmp/logs").with_profile(Profile::Debug);
        let release = LogConfig::new("/tmp/logs").with_profile(Profile::Release);

        assert!(!debug.file_enabled());
        assert!(release.file_enabled());
    }

    #[test]
    fn an_explicit_file_policy_wins_over_the_profile() {
        let debug_with_file = LogConfig::new("/tmp/logs")
            .with_profile(Profile::Debug)
            .with_file(FilePolicy::On);
        let release_without_file = LogConfig::new("/tmp/logs")
            .with_profile(Profile::Release)
            .with_file(FilePolicy::Off);

        assert!(debug_with_file.file_enabled());
        assert!(!release_without_file.file_enabled());
    }

    #[test]
    fn a_zero_ring_capacity_is_raised_to_one() {
        let config = LogConfig::new("/tmp/logs").with_ring_capacity(0);

        assert_eq!(config.ring_capacity(), 1);
    }
}
