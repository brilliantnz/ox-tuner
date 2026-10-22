//! `oxtuner` application library.
//!
//! The whole application lives in this library; the binary in `main.rs` owns the
//! start-up sequence and hands off to [`init`]. Modules are grouped by
//! capability so they can become crates later:
//!
//! - [`app`] — app-level wiring: actions, the static shortcut table, the menu,
//!   the main window's identity and geometry
//! - [`shell`] — the main window: primary sidebar, secondary sidebar, bottom
//!   panel, editable column, title bar
//! - [`settings`] — the settings window and the persisted preferences
//! - [`state`] — the persisted session document and the JSON store
//! - [`platform`] — infrastructure: filesystem paths, logging
//! - [`i18n`] — the interface language and the `t!` macro
//! - [`project`], [`disclaimer`], [`constants`] — standalone pieces
//!
//! Views live in this crate next to the state they render; the tuning core
//! arrives later as separate headless crates (ADR-0002).

// Application copy lives in the workspace-root `locales/` directory so later
// crates can reach the same files; `build.rs` makes edits rebuild this crate.
rust_i18n::i18n!("../../locales", fallback = "en");

pub mod app;
pub mod constants;
pub mod core;
pub mod disclaimer;
pub mod i18n;
pub mod platform;
pub mod project;
pub mod settings;
pub mod shell;
pub mod state;

/// Initialize GPUI Kit, the interface language, the keymap and the app-level
/// action handlers. Call once inside `Application::run`, before creating any
/// window.
pub fn init(cx: &mut gpui_kit::App) {
    // The identity the system shows notifications under, set before any window
    // opens: on Windows the AppUserModelID.
    cx.set_app_identity(app::app_identity(), "OxTuner");
    app::init(cx);
}
