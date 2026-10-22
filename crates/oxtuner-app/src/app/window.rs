//! The main window: its identity, its geometry and where its handle lives.
//!
//! Window-dependent commands (About, the disclaimer) look the window up here
//! instead of capturing a handle when they are registered, so a handler that
//! was registered before the window existed still works, and one registered
//! while a window was open does not keep a dead handle.

use gpui_kit::{
    AnyWindowHandle, App, Global, WeakEntity, Window, WindowBounds, WindowDecorations,
    WindowOptions, px, size,
};

use crate::{
    constants::APP_NAME,
    settings::preferences::AppSettings,
    shell::{Shell, titlebar::titlebar_options},
    state::session::{self, AppState},
};

/// The main window and the view inside it, once it exists.
struct MainWindow {
    handle: AnyWindowHandle,
    view: WeakEntity<Shell>,
}

impl Global for MainWindow {}

/// The main window's handle, if it is open.
pub fn main(cx: &App) -> Option<AnyWindowHandle> {
    cx.try_global::<MainWindow>().map(|window| window.handle)
}

/// Records the window the app just opened.
pub(crate) fn set(cx: &mut App, handle: AnyWindowHandle, view: &gpui_kit::Entity<Shell>) {
    cx.set_global(MainWindow {
        handle,
        view: view.downgrade(),
    });
}

/// Repaints the shell, not every window.
///
/// Panel toggles and project changes are the shell's own state; repainting the
/// Settings window for them would be wasted work.
///
/// Deferred on purpose: the toggle buttons run inside the shell's own update, so
/// updating that entity again from there would panic on the double borrow.
pub(crate) fn notify_main(cx: &mut App) {
    cx.defer(|cx| {
        let view = cx
            .try_global::<MainWindow>()
            .and_then(|window| window.view.upgrade());

        match view {
            Some(view) => {
                view.update(cx, |_, cx| cx.notify());
            }
            // No shell to notify (a stale weak handle): fall back to every
            // window so a missed repaint cannot leave the app looking frozen.
            None => cx.refresh_windows(),
        }
    });
}

/// What the window's close button does.
///
/// macOS convention, and what a single-window app needs: closing the window
/// hides the app instead of leaving a running app with no window and no way to
/// bring one back. The Dock icon or ⌘Tab restores it; ⌘Q quits. Elsewhere the
/// window closes as usual, which quits once it was the last one.
///
/// A window that owns the app's only view must not be destroyed by
/// its own close button.
pub fn install_close_behavior(window: &Window, cx: &App) {
    // Both platforms record the geometry here rather than only in the quit hook:
    // on macOS closing hides instead of quitting, and elsewhere the window is
    // already gone by the time the app quits, so `main` can no longer be asked.
    window.on_window_should_close(cx, |_, cx| {
        save_window_state(cx);
        AppState::save(cx);

        // macOS: hide, keeping the app alive so the Dock icon brings it back.
        #[cfg(target_os = "macos")]
        cx.hide();

        #[cfg(target_os = "macos")]
        return false;

        // Windows and Linux: let it close; GPUI quits when the last window goes.
        #[cfg(not(target_os = "macos"))]
        true
    });
}

/// Brings the hidden main window back, in front of other apps.
///
/// Activates the app as well as the window: a hidden app's windows stay hidden
/// even after `activate_window`.
pub fn bring_forward(window: &mut Window, cx: &mut App) {
    cx.activate(true);
    window.activate_window();
}

/// Options for the main window, restoring the geometry the user left.
///
/// The window is **always** created windowed, at a size guaranteed to be smaller
/// than the display (see `WindowState::windowed_bounds`); [`restore`] applies the
/// last session's maximize afterwards. Both halves matter:
///
/// - creating with `WindowBounds::Maximized` asks the platform to zoom while the
///   window is still being created, before it is on screen, which is too early;
/// - creating at the *maximized* size and then zooming is worse: on macOS zoom is
///   a toggle, so a window that already covers the display is shrunk by it.
pub fn options(cx: &App) -> WindowOptions {
    let bounds = cx.global::<AppState>().data().window.windowed_bounds(cx);

    WindowOptions {
        // No native caption: AppKit keeps the traffic lights but draws no title
        // strip, background or separator, and the content view covers the whole
        // window. `titlebar.rs` draws the bar instead. The title stays set
        // because Mission Control and the Window menu read it.
        titlebar: Some(titlebar_options(APP_NAME)),
        // The bar owns dragging and double-click zoom, not AppKit; without
        // this, macOS also handles title-bar double clicks and delays clicks
        // while disambiguating them.
        app_owns_titlebar_drag: true,
        // Linux only: ask for client-side decorations, since this window draws
        // its own caption. Left unset, Wayland compositors supply a
        // server-side title bar and the window ends up with two; X11 falls back
        // to the window manager's frame on its own when no compositor runs.
        // macOS and Windows ignore this field.
        window_decorations: Some(WindowDecorations::Client),
        // Always windowed: [`restore`] applies the last session's maximize once
        // the window exists.
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(880.), px(560.))),
        app_id: Some(APP_NAME.into()),
        ..Default::default()
    }
}

/// How long after opening the window geometry writes are ignored.
///
/// Restoring a window is asynchronous: the platform applies the frame and the
/// zoom after the window exists, so the bounds observer sees the windowed frame
/// first. Writing that would be exactly the "maximized session reopens windowed"
/// bug, whatever the restore mechanism.
const RESTORE_GRACE: std::time::Duration = std::time::Duration::from_millis(1200);

/// When the maximize request is repeated, in case the window manager is still
/// mapping the window when the first one arrives. Windows and Linux only.
#[cfg(not(target_os = "macos"))]
const RETRY_FIRST: std::time::Duration = std::time::Duration::from_millis(250);
#[cfg(not(target_os = "macos"))]
const RETRY_SECOND: std::time::Duration = std::time::Duration::from_millis(500);

/// Restores the window the last session left.
///
/// The window was created windowed at a size smaller than the display, so a zoom
/// request here always expands it — which is why macOS's toggle semantics are
/// safe. Asking for it during creation is too early for the platform; asking with
/// `restore_native_window_state` instead is worse: AppKit applies that
/// synchronously, its frame change re-enters the app through GPUI's resize
/// callback, and the app is already mutably borrowed ("RefCell already borrowed").
/// `zoom_window()` is asynchronous, so it never re-enters.
///
/// Windows and Linux retry, because their window managers may still be mapping the
/// window. macOS does not: a second zoom would un-zoom it.
pub fn restore(window: &mut Window, cx: &mut App) {
    session::begin_restore(RESTORE_GRACE);

    // Heal a maximize-shaped record before using it. Such a rectangle can only
    // come from a frame a maximize left behind (the zoomed frame itself, or the
    // "standard frame" AppKit produces when zooming a window that already fills
    // the display), so the last session *was* maximized even when the flag says
    // otherwise. Dropping the rectangle and keeping the flag turn a stuck
    // near-full window back into "open at a usable size, then maximize".
    if cx
        .global::<AppState>()
        .data()
        .window
        .bounds_are_maximize_shaped(cx)
    {
        tracing::info!("window: healing a maximize-shaped record, restoring maximized");
        AppState::update_global(cx, |state| {
            state.window.bounds = None;
            state.window.maximized = true;
        });
        AppState::save(cx);
    }

    let maximized = cx.global::<AppState>().data().window.maximized;
    tracing::info!(
        maximized,
        width = window.bounds().size.width.as_f32(),
        height = window.bounds().size.height.as_f32(),
        "window: restoring the last session"
    );

    if !maximized {
        record_settled_geometry(window, cx);
        return;
    }

    window.zoom_window();

    #[cfg(not(target_os = "macos"))]
    retry_until_maximized(window, cx);

    record_settled_geometry(window, cx);
}

/// Records the geometry once start-up has settled.
///
/// The bounds observer ignores everything during [`RESTORE_GRACE`], so without
/// this a run where the user never moves or resizes the window would store
/// nothing: a first launch would have no windowed size to return to, and a
/// maximized session would keep whatever was there before. Runs after the grace
/// has expired so the restored frame is what gets written.
fn record_settled_geometry(window: &Window, cx: &mut App) {
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(RESTORE_GRACE + std::time::Duration::from_millis(100))
            .await;
        cx.update(|cx| {
            let recorded = handle
                .update(cx, |_, window, cx| {
                    session::capture_window(window, cx);
                })
                .is_ok();
            if recorded {
                session::schedule_save(cx);
            }
        });
    })
    .detach();
}

/// Repeats the maximize request until the platform agrees.
///
/// X11 and Wayland implement the request as a **toggle**, so each attempt is
/// guarded by `is_maximized`: a settled window is never toggled back down.
#[cfg(not(target_os = "macos"))]
fn retry_until_maximized(window: &mut Window, cx: &mut App) {
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        for delay in [RETRY_FIRST, RETRY_SECOND] {
            cx.background_executor().timer(delay).await;
            let still_waiting = cx.update(|cx| {
                handle
                    .update(cx, |_, window, cx| {
                        if window.is_maximized() {
                            // Settled; record what the platform now agrees with.
                            session::capture_window(window, cx);
                            return false;
                        }
                        window.zoom_window();
                        true
                    })
                    .unwrap_or(false)
            });
            if !still_waiting {
                return;
            }
        }
    })
    .detach();
}

/// Saves both documents when the app quits.
///
/// The live window is asked for its geometry first: the bounds observer runs a
/// frame behind, and quitting right after a zoom must not store the previous
/// state. Then both files are written, because the debounced writer may still be
/// waiting.
pub fn save_on_quit(cx: &mut App) {
    let _quit = cx.on_app_quit(|cx| {
        save_window_state(cx);
        AppState::save(cx);
        AppSettings::save(cx);
        // The core stops before the process tears down, so its thread is
        // joined while the runtime it reports into still exists.
        crate::core::request_shutdown(cx);
        async {}
    });
}

/// Copies the live window into the session document.
///
/// Needed before anything that may end the process — quitting, hiding on close —
/// because both happen outside a frame where the debounced writer would run.
pub(crate) fn save_window_state(cx: &mut App) {
    if let Some(handle) = main(cx) {
        handle
            .update(cx, |_, window, cx| session::capture_window(window, cx))
            .ok();
    }
}
