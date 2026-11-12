//! Per-OS application directories.
//!
//! Every path the app owns is derived here, never by joining `~/` by hand. The
//! platform-specific layout is a pure function of [`PlatformDirs`], so the
//! macOS, Windows and Linux layouts are testable on any one of them.
//!
//! Settings and session state are user data and live in the config directory;
//! logs are diagnostics and live in the platform's state/log directory. The
//! project itself is never copied — the app only remembers its path.

use std::path::{Path, PathBuf};

/// The operating-system layout a set of base directories belongs to.
#[expect(
    clippy::exhaustive_enums,
    reason = "the app ships for exactly these three desktop platforms"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// macOS.
    Macos,
    /// Windows.
    Windows,
    /// Linux.
    Linux,
}

impl Platform {
    /// The platform this binary was built for.
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::Macos
        } else if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Linux
        }
    }
}

/// The base directories the platform hands us, before the app name is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlatformDirs {
    pub(crate) config_base: PathBuf,
    pub(crate) data_local_base: PathBuf,
    pub(crate) cache_base: PathBuf,
    pub(crate) home: PathBuf,
}

impl PlatformDirs {
    /// Reads the current machine's base directories.
    pub(crate) fn discover() -> Option<Self> {
        let base = directories::BaseDirs::new()?;
        Some(Self {
            config_base: base.config_dir().to_path_buf(),
            data_local_base: base.data_local_dir().to_path_buf(),
            cache_base: base.cache_dir().to_path_buf(),
            home: base.home_dir().to_path_buf(),
        })
    }
}

/// The directories ox-tuner reads and writes.
#[expect(
    clippy::struct_field_names,
    reason = "each field names one of the app's directories"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub(crate) config_dir: PathBuf,
    pub(crate) cache_dir: PathBuf,
    pub(crate) logs_dir: PathBuf,
}

impl AppPaths {
    /// Resolves the paths for this machine.
    ///
    /// If the platform cannot report a home directory, fall back to a relative
    /// directory rather than panicking: an app that cannot find a place for its
    /// settings should still start.
    pub fn discover() -> Self {
        PlatformDirs::discover().map_or_else(
            || Self {
                config_dir: PathBuf::from(".oxtuner"),
                cache_dir: PathBuf::from(".oxtuner/cache"),
                logs_dir: PathBuf::from(".oxtuner/logs"),
            },
            |dirs| Self::resolve(Platform::current(), &dirs),
        )
    }

    /// Applies the application directory names to a platform's bases.
    ///
    /// macOS uses the display-style `OxTuner` in Application Support, Caches
    /// and Logs. Windows uses `OxTuner` under Roaming (settings), Local (cache
    /// and logs). Linux follows the XDG convention: lowercase `oxtuner` under
    /// `~/.config`, `~/.cache` and `~/.local/state`.
    pub(crate) fn resolve(platform: Platform, dirs: &PlatformDirs) -> Self {
        match platform {
            Platform::Macos => Self {
                config_dir: dirs.config_base.join("OxTuner"),
                cache_dir: dirs.cache_base.join("OxTuner"),
                logs_dir: dirs.home.join("Library/Logs/OxTuner"),
            },
            Platform::Windows => Self {
                config_dir: dirs.config_base.join("OxTuner"),
                cache_dir: dirs.data_local_base.join("OxTuner").join("cache"),
                logs_dir: dirs.data_local_base.join("OxTuner").join("logs"),
            },
            Platform::Linux => Self {
                config_dir: dirs.config_base.join("oxtuner"),
                cache_dir: dirs.cache_base.join("oxtuner"),
                logs_dir: dirs.home.join(".local/state/oxtuner/logs"),
            },
        }
    }

    /// The user-editable application settings.
    pub fn settings_file(&self) -> PathBuf {
        self.config_dir.join("settings.json")
    }

    /// Window geometry, panel sizes and the last project.
    pub fn state_file(&self) -> PathBuf {
        self.config_dir.join("state.json")
    }

    /// Parsed definitions, keyed by their source fingerprints.
    ///
    /// A cache, not user data: deleting it costs one parse per definition and
    /// changes nothing else.
    pub fn definition_cache_dir(&self) -> PathBuf {
        self.cache_dir.join("definitions")
    }

    /// The directory that holds the user-editable color themes. The bundled
    /// `OxTuner` light and dark themes are seeded here on first launch, next to
    /// `settings.json` and `state.json`, and reloaded live when edited.
    pub fn themes_dir(&self) -> PathBuf {
        self.config_dir.join("themes")
    }

    /// The directory for per-run log files.
    pub fn log_dir(&self) -> &Path {
        &self.logs_dir
    }

    /// Session traces, next to the logs: they are diagnostics, not user data.
    pub fn traces_dir(&self) -> PathBuf {
        self.logs_dir.join("traces")
    }

    /// A fresh trace path for this session.
    ///
    /// A timestamp names the file, so two sessions never overwrite each other
    /// and the bench run that produced a trace is identifiable later.
    pub fn next_trace_path(&self) -> PathBuf {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis());
        self.traces_dir().join(format!("session-{millis}.trace"))
    }

    /// Installs the resolved paths for the process.
    ///
    /// The binary discovers them once, before the first window, so later code
    /// reads the same instance instead of asking the platform again.
    pub fn install(cx: &mut gpui_kit::App, paths: Self) {
        cx.set_global(paths);
    }

    /// The paths installed by [`Self::install`].
    ///
    /// # Panics
    ///
    /// Panics when called before [`Self::install`]. The binary installs them
    /// before any command can run.
    pub fn get(cx: &gpui_kit::App) -> &Self {
        cx.global::<Self>()
    }
}

impl gpui_kit::Global for AppPaths {}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(config: &str, local: &str, cache: &str, home: &str) -> PlatformDirs {
        PlatformDirs {
            config_base: PathBuf::from(config),
            data_local_base: PathBuf::from(local),
            cache_base: PathBuf::from(cache),
            home: PathBuf::from(home),
        }
    }

    #[test]
    fn macos_uses_application_support_and_library_logs() {
        let dirs = dirs(
            "/Users/me/Library/Application Support",
            "/Users/me/Library/Application Support",
            "/Users/me/Library/Caches",
            "/Users/me",
        );
        let paths = AppPaths::resolve(Platform::Macos, &dirs);

        assert_eq!(
            paths.config_dir,
            PathBuf::from("/Users/me/Library/Application Support/OxTuner")
        );
        assert_eq!(
            paths.logs_dir,
            PathBuf::from("/Users/me/Library/Logs/OxTuner")
        );
        assert_eq!(
            paths.cache_dir,
            PathBuf::from("/Users/me/Library/Caches/OxTuner")
        );
        assert_eq!(
            paths.definition_cache_dir(),
            PathBuf::from("/Users/me/Library/Caches/OxTuner/definitions")
        );
        assert_eq!(
            paths.settings_file(),
            PathBuf::from("/Users/me/Library/Application Support/OxTuner/settings.json")
        );
        assert_eq!(
            paths.state_file(),
            PathBuf::from("/Users/me/Library/Application Support/OxTuner/state.json")
        );
        assert_eq!(
            paths.themes_dir(),
            PathBuf::from("/Users/me/Library/Application Support/OxTuner/themes")
        );
    }

    #[test]
    fn windows_uses_roaming_for_settings_and_local_for_logs() {
        let dirs = dirs(
            r"C:\Users\me\AppData\Roaming",
            r"C:\Users\me\AppData\Local",
            r"C:\Users\me\AppData\Local",
            r"C:\Users\me",
        );
        let paths = AppPaths::resolve(Platform::Windows, &dirs);

        assert_eq!(
            paths.config_dir,
            PathBuf::from(r"C:\Users\me\AppData\Roaming").join("OxTuner")
        );
        assert_eq!(
            paths.cache_dir,
            PathBuf::from(r"C:\Users\me\AppData\Local")
                .join("OxTuner")
                .join("cache")
        );
        assert_eq!(
            paths.logs_dir,
            PathBuf::from(r"C:\Users\me\AppData\Local")
                .join("OxTuner")
                .join("logs")
        );
    }

    #[test]
    fn linux_follows_xdg_for_config_and_state() {
        let dirs = dirs(
            "/home/me/.config",
            "/home/me/.local/share",
            "/home/me/.cache",
            "/home/me",
        );
        let paths = AppPaths::resolve(Platform::Linux, &dirs);

        assert_eq!(paths.config_dir, PathBuf::from("/home/me/.config/oxtuner"));
        assert_eq!(paths.cache_dir, PathBuf::from("/home/me/.cache/oxtuner"));
        assert_eq!(
            paths.logs_dir,
            PathBuf::from("/home/me/.local/state/oxtuner/logs")
        );
    }
}
