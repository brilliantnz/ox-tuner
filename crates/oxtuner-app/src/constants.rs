//! Product-wide constants shared by the app's views.

/// Product name, used by the title bar and the window options in `main`.
pub(crate) const APP_NAME: &str = "Ox Tuner";

/// The main window's minimum width, in logical pixels.
///
/// The layout's sidebars and editor need this much room before they cramp, so
/// it is also the size the first window opens at: creating a smaller default
/// only makes the platform grow the window on the first frame.
///
/// This is a hard floor on every platform, so a smaller display shows the
/// window at the minimum with part of it off screen.
pub(crate) const MAIN_WINDOW_MIN_WIDTH: f32 = 1600.;

/// The main window's minimum height, in logical pixels.
pub(crate) const MAIN_WINDOW_MIN_HEIGHT: f32 = 980.;
