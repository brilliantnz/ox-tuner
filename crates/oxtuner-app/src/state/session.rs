//! Application session state: the parts of the workspace that should survive a
//! restart.
//!
//! Unlike [`crate::preferences`], this file changes on a human/OS timescale
//! (drags, window resizes, opening a project), so writes are debounced before
//! they hit disk and flushed once more on quit. The data is deliberately
//! separate from settings: losing a window position is harmless, while
//! settings are the user's explicit choices.
//!
//! The file is `<config>/state.json` — small, readable, and next to
//! `settings.json` so the whole application's local state can be inspected in
//! one place.

use std::{
    cell::Cell,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use gpui_kit::{App, Bounds, Global, Pixels, Window, point, px, size};
use oxtuner_core::ProjectLayout;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::store;
use crate::core::CoreService;
use crate::platform::paths::AppPaths;

/// The on-disk shape this build writes.
///
/// The app is unreleased, so a shape change deletes the stale file and lets it
/// be written again; schema versions and migrations arrive with the first
/// release. The number exists so a file from a newer build is refused instead
/// of half-read.
pub(crate) const STATE_SCHEMA_VERSION: u32 = 1;

/// How long to wait after the last layout/window change before writing.
///
/// A drag emits only one resize callback in gpui-kit's resizable component, but
/// window bounds can arrive in bursts while a window is being moved. Half a
/// second keeps a drag from causing a write per frame while still surviving a
/// crash with at most this much lost.
const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

/// Width limits of the primary (navigation) sidebar.
pub(crate) const PRIMARY_SIDEBAR_WIDTH_RANGE: std::ops::Range<f32> = 180.0..420.0;
/// Width limits of the secondary (inspector) sidebar.
pub(crate) const SECONDARY_SIDEBAR_WIDTH_RANGE: std::ops::Range<f32> = 200.0..480.0;
/// Height limits of the bottom panel.
pub(crate) const BOTTOM_PANEL_HEIGHT_RANGE: std::ops::Range<f32> = 180.0..480.0;

/// How close a frame must be to a display's usable area to *record* a maximize.
///
/// One pixel, not a generous band: this only absorbs the rounding a zoomed frame
/// can carry. It is never used to *decide* whether a window is maximized from a
/// stored rectangle, only to recognise the frame zoom produces.
///
/// macOS-only: `is_zoom_frame` is the only reader, and Windows and Linux get the
/// answer from `Window::is_maximized` alone.
#[cfg(target_os = "macos")]
const MAXIMIZE_EPSILON_PX: f32 = 1.;

/// How much room a stored **windowed** rectangle must leave around itself.
///
/// Zoom is not the only way to end up with a frame the size of the display.
/// `AppKit`'s `zoom:` on a window that already fills the usable area *shrinks* it to
/// the "standard frame" — measured at 1780×1110 on a 1800×1130 display, i.e.
/// exactly 20 pt of margin on the trailing edges. That frame is maximized in
/// everything but name: it cannot be told apart from a maximized window by the
/// platform's flag, and creating the window at it makes every later zoom step
/// smaller.
///
/// So a rectangle that leaves less than this around the display is never kept as
/// the windowed size. 24 pt covers `AppKit`'s 20 pt with room for a different
/// display scale, while staying far below any window a person would actually
/// choose to work in.
const WINDOWED_MIN_MARGIN_PX: f32 = 24.;

/// The JSON document at `<config>/state.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct AppStateData {
    pub(crate) schema_version: u32,
    pub(crate) window: WindowState,
    pub(crate) layout: LayoutState,
    pub(crate) last_project: Option<PathBuf>,
}

impl Default for AppStateData {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            window: WindowState::default(),
            layout: LayoutState::default(),
            last_project: None,
        }
    }
}

impl AppStateData {
    fn from_json_value(mut value: Value) -> Result<Self, String> {
        let version = value
            .get("schema_version")
            .and_then(Value::as_u64)
            .map_or(0, |version| u32::try_from(version).unwrap_or(u32::MAX));

        match version {
            STATE_SCHEMA_VERSION | 0 => {
                // A file written before the field existed picks the current
                // default through serde; nothing needs rewriting here.
            }
            version if version < STATE_SCHEMA_VERSION => {
                return Err(format!(
                    "state schema {version} is older than the supported range"
                ));
            }
            version => {
                return Err(format!(
                    "state schema {version} is newer than this build supports ({STATE_SCHEMA_VERSION})"
                ));
            }
        }

        if let Some(object) = value.as_object_mut() {
            object.insert("schema_version".into(), STATE_SCHEMA_VERSION.into());
        }

        serde_json::from_value(value).map_err(|error| error.to_string())
    }

    /// The active project folder, if one has been chosen.
    pub(crate) fn last_project(&self) -> Option<&Path> {
        self.last_project.as_deref()
    }
}

/// Window geometry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct WindowState {
    /// The last non-maximized bounds, so un-maximizing restores a useful size.
    pub(crate) bounds: Option<WindowRect>,
    pub(crate) maximized: bool,
}

impl WindowState {
    /// Whether the stored rectangle is a **maximize-shaped** frame recorded as
    /// windowed geometry.
    ///
    /// Such a rectangle is never usable: creating the window at it and then
    /// zooming does not enlarge it, and the next zoom step produces a slightly
    /// smaller frame, so the window ratchets down launch after launch. It is
    /// cleared on start-up so the file heals as well as the window.
    pub(crate) fn bounds_are_maximize_shaped(&self, cx: &App) -> bool {
        self.bounds
            .map(WindowRect::to_bounds)
            .is_some_and(|stored| {
                cx.displays()
                    .iter()
                    .any(|display| fills_display(&display.visible_bounds(), &stored))
            })
    }

    /// The geometry the window takes while it is **windowed**.
    ///
    /// This is the size the window is created with, and the size zooming out
    /// returns to, so it must be a size a person could work in — never one of the
    /// maximize-shaped frames [`Self::bounds_are_maximize_shaped`] rejects, which
    /// would be replaced by [`Self::default_bounds`].
    ///
    /// A rectangle whose display is gone (the monitor was unplugged) is kept but
    /// re-centred on the current one.
    pub(crate) fn windowed_bounds(&self, cx: &App) -> Bounds<Pixels> {
        if self.bounds_are_maximize_shaped(cx) {
            return Self::default_bounds(cx);
        }

        let Some(stored) = self.bounds.map(WindowRect::to_bounds) else {
            return Self::default_bounds(cx);
        };

        let on_a_display = cx
            .displays()
            .iter()
            .any(|display| overlap_area(&display.visible_bounds(), &stored) > 0.);
        if on_a_display {
            stored
        } else {
            Bounds::centered(None, stored.size, cx)
        }
    }

    /// The designed first-run size: 1200×800, centred, which leaves visible space
    /// around it on a laptop display.
    pub(crate) fn default_bounds(cx: &App) -> Bounds<Pixels> {
        Bounds::centered(None, size(px(1200.), px(800.)), cx)
    }
}

/// Whether `window` is **maximize-shaped**: it leaves less than
/// [`WINDOWED_MIN_MARGIN_PX`] of the display uncovered in both axes.
///
/// This is the storage rule, not a maximize test. It catches every frame a
/// maximize can leave behind — the zoomed frame itself, and `AppKit`'s "standard
/// frame" (usable area minus 20 pt, what a window that already fills the display
/// becomes when zoomed) — without ever deciding from geometry that a window *is*
/// maximized.
fn fills_display(display: &Bounds<Pixels>, window: &Bounds<Pixels>) -> bool {
    let nearly = |window: Pixels, display: Pixels| {
        window.as_f32() >= display.as_f32() - WINDOWED_MIN_MARGIN_PX
    };

    nearly(window.size.width, display.size.width) && nearly(window.size.height, display.size.height)
}

/// Whether `window` *is* the frame zoom produces: the display's usable area at the
/// display's origin, within `MAXIMIZE_EPSILON_PX`.
///
/// macOS-only: gpui's platform check compares sizes exactly, so a zoomed window
/// can read as "not maximized" there. Windows and Linux answer the question
/// themselves.
#[cfg(target_os = "macos")]
fn is_zoom_frame(display: &Bounds<Pixels>, window: &Bounds<Pixels>) -> bool {
    let close = |a: Pixels, b: Pixels| (a.as_f32() - b.as_f32()).abs() <= MAXIMIZE_EPSILON_PX;

    close(window.origin.x, display.origin.x)
        && close(window.origin.y, display.origin.y)
        && close(window.size.width, display.size.width)
        && close(window.size.height, display.size.height)
}

/// How much of `window` lands inside `display`, in square pixels.
fn overlap_area(display: &Bounds<Pixels>, window: &Bounds<Pixels>) -> f32 {
    let overlap = display.intersect(window).size;
    (overlap.width.as_f32() * overlap.height.as_f32()).max(0.)
}

/// A window rectangle in device-independent pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct WindowRect {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
}

impl WindowRect {
    pub(crate) fn from_bounds(bounds: &Bounds<Pixels>) -> Self {
        Self {
            x: bounds.origin.x.as_f32(),
            y: bounds.origin.y.as_f32(),
            width: bounds.size.width.as_f32(),
            height: bounds.size.height.as_f32(),
        }
    }

    pub(crate) fn to_bounds(self) -> Bounds<Pixels> {
        Bounds::new(
            point(px(self.x), px(self.y)),
            size(px(self.width.max(640.)), px(self.height.max(480.))),
        )
    }
}

/// Sizes and visibility of the three main layout panels, in pixels.
///
/// The panels are independent: any of them can be hidden, and the editor
/// absorbs whatever space is left.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each panel visibility is an independent layout fact"
)]
pub(crate) struct LayoutState {
    pub(crate) primary_sidebar_width: f32,
    pub(crate) secondary_sidebar_width: f32,
    pub(crate) bottom_panel_height: f32,
    /// Whether the navigation sidebar is open.
    #[serde(default = "default_true")]
    pub(crate) primary_sidebar_visible: bool,
    /// Whether the inspector sidebar is open.
    #[serde(default = "default_true")]
    pub(crate) secondary_sidebar_visible: bool,
    /// Whether the output panel is open. Missing in older files, where it did
    /// not exist; `serde(default)` on a struct field would use `false`, so the
    /// default is named explicitly.
    #[serde(default = "default_true")]
    pub(crate) bottom_panel_visible: bool,
    /// Whether the table grid draws its Y axis downward: low bins at the top.
    ///
    /// The default draws it upward like a chart, so the low bin is at the
    /// bottom and the column header sits under the table, the way a
    /// reference editor opens a table.
    ///
    /// This one is **not** stored in `state.json`: how a table is drawn belongs
    /// to the project whose table it is, so it lives in the project's own
    /// `project.json` and is echoed here only while that project is open. A
    /// standalone definition has no state file, so it starts from the default.
    #[serde(skip)]
    pub(crate) table_low_at_top: bool,
}

const fn default_true() -> bool {
    true
}

impl Default for LayoutState {
    fn default() -> Self {
        Self {
            primary_sidebar_width: ProjectLayout::DEFAULT_PRIMARY_SIDEBAR_WIDTH,
            secondary_sidebar_width: ProjectLayout::DEFAULT_SECONDARY_SIDEBAR_WIDTH,
            bottom_panel_height: ProjectLayout::DEFAULT_BOTTOM_PANEL_HEIGHT,
            primary_sidebar_visible: true,
            secondary_sidebar_visible: true,
            bottom_panel_visible: true,
            table_low_at_top: false,
        }
    }
}

impl LayoutState {
    pub(crate) fn primary_sidebar(&self) -> Pixels {
        px(clamp(
            self.primary_sidebar_width,
            PRIMARY_SIDEBAR_WIDTH_RANGE,
        ))
    }

    pub(crate) fn secondary_sidebar(&self) -> Pixels {
        px(clamp(
            self.secondary_sidebar_width,
            SECONDARY_SIDEBAR_WIDTH_RANGE,
        ))
    }

    pub(crate) fn bottom_panel(&self) -> Pixels {
        px(clamp(self.bottom_panel_height, BOTTOM_PANEL_HEIGHT_RANGE))
    }

    /// The project state this layout is stored as.
    pub(crate) fn to_project_layout(&self) -> ProjectLayout {
        ProjectLayout {
            primary_sidebar_width: self.primary_sidebar_width,
            secondary_sidebar_width: self.secondary_sidebar_width,
            bottom_panel_height: self.bottom_panel_height,
            primary_sidebar_visible: self.primary_sidebar_visible,
            secondary_sidebar_visible: self.secondary_sidebar_visible,
            bottom_panel_visible: self.bottom_panel_visible,
            table_low_at_top: self.table_low_at_top,
        }
    }

    /// Takes the panel layout a project stored.
    ///
    /// The stored numbers are used as they are: [`Self::primary_sidebar`] and
    /// the other readers clamp them, so a file edited by hand cannot open a
    /// panel at an unusable size.
    pub(crate) fn apply_project_layout(&mut self, layout: &ProjectLayout) {
        self.primary_sidebar_width = layout.primary_sidebar_width;
        self.secondary_sidebar_width = layout.secondary_sidebar_width;
        self.bottom_panel_height = layout.bottom_panel_height;
        self.primary_sidebar_visible = layout.primary_sidebar_visible;
        self.secondary_sidebar_visible = layout.secondary_sidebar_visible;
        self.bottom_panel_visible = layout.bottom_panel_visible;
        self.table_low_at_top = layout.table_low_at_top;
    }

    /// Records the width the outer group reports for its first panel: the
    /// primary sidebar.
    ///
    /// Short slices are ignored rather than indexed: the group reports whatever
    /// panels it currently holds, and a panel can be hidden.
    pub(crate) fn apply_primary_sidebar(&mut self, sizes: &[Pixels]) {
        if let Some(width) = sizes.first() {
            self.primary_sidebar_width = width.as_f32();
        }
    }

    /// Records the width the panel group reports for its second panel: the
    /// secondary sidebar.
    pub(crate) fn apply_secondary_sidebar(&mut self, sizes: &[Pixels]) {
        if let Some(width) = sizes.get(1) {
            self.secondary_sidebar_width = width.as_f32();
        }
    }

    /// Records the height the editor group reports for its second panel: the
    /// bottom panel.
    pub(crate) fn apply_bottom_panel(&mut self, sizes: &[Pixels]) {
        if let Some(height) = sizes.get(1) {
            self.bottom_panel_height = height.as_f32();
        }
    }
}

fn clamp(value: f32, range: std::ops::Range<f32>) -> f32 {
    value.clamp(range.start, range.end)
}

/// The parsed session document plus the file it came from.
#[derive(Debug)]
pub struct AppState {
    path: PathBuf,
    data: AppStateData,
}

impl Global for AppState {}

impl AppState {
    /// Loads `<config>/state.json`, or starts from defaults when it is missing
    /// or unreadable. An unreadable file is moved aside before defaults replace
    /// it.
    pub fn load(paths: &AppPaths) -> Self {
        let path = paths.state_file();
        let data = match store::read_json::<Value>(&path) {
            Ok(Some(value)) => match AppStateData::from_json_value(value) {
                Ok(data) => data,
                Err(error) => {
                    tracing::warn!(%error, path = %path.display(), "state file is invalid");
                    if let Err(move_error) = store::quarantine(&path) {
                        tracing::warn!(error = %move_error, "could not move invalid state aside");
                    }
                    AppStateData::default()
                }
            },
            Ok(None) => AppStateData::default(),
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "could not read state");
                if let Err(move_error) = store::quarantine(&path) {
                    tracing::warn!(error = %move_error, "could not move invalid state aside");
                }
                AppStateData::default()
            }
        };

        Self { path, data }
    }

    pub(crate) fn data(&self) -> &AppStateData {
        &self.data
    }

    pub(crate) fn last_project(&self) -> Option<&Path> {
        self.data.last_project()
    }

    /// Writes the current document synchronously (project opened, app quitting).
    ///
    /// A debounced write already in flight may rename after this one and leave
    /// an older snapshot behind. That is acceptable: the state file is a
    /// convenience, and a lost window position costs one restore.
    pub(crate) fn save(cx: &App) {
        let global = cx.global::<AppState>();
        if let Err(error) = store::write_json(&global.path, &global.data) {
            tracing::error!(
                %error,
                path = %global.path.display(),
                "could not save application state"
            );
        }
    }

    /// Mutates the session document. The caller decides when to persist.
    pub(crate) fn update_global(cx: &mut App, mutate: impl FnOnce(&mut AppStateData)) {
        let global = cx.global_mut::<Self>();
        mutate(&mut global.data);
        global.data.schema_version = STATE_SCHEMA_VERSION;
    }

    /// Replaces the in-memory document and path; used by tests.
    #[cfg(test)]
    fn with(data: AppStateData) -> Self {
        Self {
            path: PathBuf::from("/tmp/oxtuner-state.json"),
            data,
        }
    }
}

/// Copies the current window geometry into the session document.
///
/// Ignored while a start-up restore is in flight ([`begin_restore`]): the first
/// bounds events after opening describe the transient windowed frame, and saving
/// that is exactly how a maximized session used to reopen as an ordinary window.
///
/// A maximize-shaped frame is never stored as the windowed size, even when the
/// platform's maximize flag says otherwise. That combination — flag false, frame
/// the size of the display — is what made a maximized session reopen smaller: the
/// window was created at that size, and then `zoom_window()` produced a *smaller*
/// frame instead of enlarging it.
pub(crate) fn capture_window(window: &Window, cx: &mut App) {
    if restoring() {
        return;
    }

    let maximized = window_is_maximized(window, cx);
    let bounds = window.bounds();
    let maximize_shaped = cx
        .displays()
        .iter()
        .any(|display| fills_display(&display.visible_bounds(), &bounds));

    // Worth a line in the log — this is the state that used to be persisted, and
    // the only way to tell a platform artifact from a real window size when
    // someone reports the window "coming back smaller". Logged on the transition
    // only: a maximized window still emits bounds events while it is dragged.
    if maximize_shaped != MAXIMIZE_SHAPED_SEEN.with(Cell::get) {
        MAXIMIZE_SHAPED_SEEN.with(|seen| seen.set(maximize_shaped));
        if maximize_shaped {
            tracing::debug!(
                width = bounds.size.width.as_f32(),
                height = bounds.size.height.as_f32(),
                maximized,
                "window: maximize-shaped frame, keeping the stored windowed size"
            );
        }
    }

    AppState::update_global(cx, |state| {
        state.window.maximized = maximized;
        if !maximized && !maximize_shaped {
            state.window.bounds = Some(WindowRect::from_bounds(&bounds));
        }
    });
}

/// Whether the window is maximized right now.
///
/// `Window::is_maximized` is the primary signal, and the only one Windows and
/// Linux need: it asks `IsZoomed` and the window manager's state respectively.
///
/// macOS additionally accepts the frame zoom produces — the display's usable area
/// at its origin, within one pixel — because gpui's macOS check compares sizes
/// exactly and misses a zoomed window by a pixel often enough that the flag was
/// saved as `false`. This *records* the state; nothing infers a maximize from a
/// size when opening.
///
/// `cx` is only read on macOS. The parameter stays in the signature on every
/// platform so callers do not need their own `cfg`, and the allowance below keeps
/// `-D warnings` happy where it is unused.
#[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
pub(crate) fn window_is_maximized(window: &Window, cx: &App) -> bool {
    if window.is_maximized() {
        return true;
    }

    #[cfg(target_os = "macos")]
    if let Some(display) = window.display(cx) {
        return is_zoom_frame(&display.visible_bounds(), &window.bounds());
    }

    false
}

thread_local! {
    /// Deadline until which window-geometry writes are ignored.
    static RESTORE_GRACE_UNTIL: Cell<Option<Instant>> = const { Cell::new(None) };
    /// Whether the previous geometry capture saw a maximize-shaped frame, so the
    /// log line about it is written once per transition instead of per event.
    static MAXIMIZE_SHAPED_SEEN: Cell<bool> = const { Cell::new(false) };
}

/// Ignore window-geometry writes for `grace`, while start-up restores the window.
///
/// Restoring is asynchronous: the platform applies the frame and the zoom after
/// the window exists, and the bounds observer sees the windowed frame in between.
/// Those events are not the user's window state, so they must not be saved.
pub(crate) fn begin_restore(grace: Duration) {
    RESTORE_GRACE_UNTIL.with(|until| until.set(Some(Instant::now() + grace)));
}

fn restoring() -> bool {
    RESTORE_GRACE_UNTIL.with(|until| match until.get() {
        Some(deadline) if Instant::now() < deadline => true,
        Some(_) => {
            until.set(None);
            false
        }
        None => false,
    })
}

thread_local! {
    /// The newest requested write. GPUI runs the app and every `cx.update` on
    /// the main thread, so a thread-local is the right scope for it — and unlike
    /// an app global it cannot be missing at the moment a drag fires.
    static SAVE_GENERATION: Cell<u64> = const { Cell::new(0) };
    /// Whether a debounce task is already waiting to write.
    static SAVE_PENDING: Cell<bool> = const { Cell::new(false) };
    /// The last layout sent to the core, keyed by project. Core conversations
    /// are strictly sequential, so a layout that did not move must not occupy
    /// the loop a drag never touched — only a change earns a command.
    static SENT_LAYOUT: std::cell::RefCell<Option<(PathBuf, ProjectLayout)>> =
        const { std::cell::RefCell::new(None) };
}

/// Schedules a debounced state write.
///
/// Every call bumps a generation; the waiting task writes once the burst has
/// gone quiet, and writes again if changes arrived while it was writing.
///
/// Only one task ever waits. A live window resize reports bounds changes every
/// frame, and spawning a fresh timer task for each one kept the executor busy
/// on top of the layout work — which showed up as the panels stuttering while
/// the window was being dragged.
pub(crate) fn schedule_save(cx: &mut App) {
    SAVE_GENERATION.with(|next| next.set(next.get().wrapping_add(1)));

    if SAVE_PENDING.with(Cell::get) {
        return;
    }
    SAVE_PENDING.with(|pending| pending.set(true));

    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let generation = SAVE_GENERATION.with(Cell::get);

            // The snapshot is taken on the UI thread so the document stays
            // coherent. The flush and rename then run on the background
            // executor: `sync_all` can stall on a busy disk, and the UI thread
            // has no business waiting for it. The layout also belongs to the
            // open project, so it is captured with the same snapshot.
            let (path, data, project_snapshot) = cx.update(|cx| {
                let global = cx.global::<AppState>();
                let project_snapshot = CoreService::status(cx)
                    .project
                    .as_ref()
                    .map(|project| (project.root.clone(), global.data.layout.to_project_layout()));
                (global.path.clone(), global.data.clone(), project_snapshot)
            });
            let (path, result) = cx
                .background_executor()
                .spawn(async move {
                    let result = store::write_json(&path, &data);
                    (path, result)
                })
                .await;
            if let Err(error) = result {
                tracing::error!(
                    %error,
                    path = %path.display(),
                    "could not save application state"
                );
            }
            if let Some((root, layout)) = project_snapshot {
                // The snapshot was taken up to a debounce interval ago: a
                // project switch in that window would write project A's
                // layout into B's state file. Only the open project's own
                // root may receive it.
                let fresh = SENT_LAYOUT
                    .with(|last| last.borrow().as_ref() != Some(&(root.clone(), layout.clone())));
                if fresh {
                    let accepted = cx.update(|cx| {
                        let still_open = CoreService::status(cx)
                            .project
                            .as_ref()
                            .is_some_and(|project| project.root == root);
                        still_open && crate::core::save_project_layout(cx, layout.clone())
                    });

                    // Recorded only after the send was accepted, so a refused
                    // command is retried on the next change instead of being
                    // remembered as stored.
                    if accepted {
                        SENT_LAYOUT.with(|last| *last.borrow_mut() = Some((root, layout)));
                    }
                }
            }

            // Checked and cleared with no await in between, so a change that
            // arrives later starts a new task rather than being swallowed.
            if SAVE_GENERATION.with(Cell::get) == generation {
                SAVE_PENDING.with(|pending| pending.set(false));
                return;
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn state_round_trips_through_json() {
        let data = AppStateData {
            schema_version: STATE_SCHEMA_VERSION,
            window: WindowState {
                bounds: Some(WindowRect {
                    x: 80.,
                    y: 60.,
                    width: 1280.,
                    height: 800.,
                }),
                maximized: false,
            },
            layout: LayoutState {
                primary_sidebar_width: 300.,
                secondary_sidebar_width: 320.,
                bottom_panel_height: 220.,
                primary_sidebar_visible: true,
                secondary_sidebar_visible: false,
                bottom_panel_visible: false,
                table_low_at_top: true,
            },
            last_project: Some(PathBuf::from("/projects/roadster")),
        };

        let encoded = serde_json::to_string(&data).expect("serialize");
        let decoded: AppStateData = serde_json::from_str(&encoded).expect("deserialize");

        // The table orientation is per project, so it is the one field the
        // global file does not carry.
        assert!(
            !encoded.contains("table_low_at_top"),
            "the table orientation is not global: {encoded}"
        );
        assert!(!decoded.layout.table_low_at_top);
        assert_eq!(
            AppStateData {
                layout: LayoutState {
                    table_low_at_top: false,
                    ..data.layout
                },
                ..data
            },
            decoded
        );
    }

    #[test]
    fn the_state_file_names_the_layout_panels() {
        let encoded = serde_json::to_string(&LayoutState::default()).expect("serialize");

        for key in [
            "\"primary_sidebar_width\"",
            "\"secondary_sidebar_width\"",
            "\"bottom_panel_height\"",
            "\"primary_sidebar_visible\"",
            "\"secondary_sidebar_visible\"",
            "\"bottom_panel_visible\"",
        ] {
            assert!(encoded.contains(key), "{key} is written: {encoded}");
        }
        for key in [
            "\"primary_width\"",
            "\"secondary_width\"",
            "\"bottom_height\"",
            "\"primary_visible\"",
            "\"secondary_visible\"",
            "\"bottom_visible\"",
            "\"table_low_at_top\"",
        ] {
            assert!(
                !encoded.contains(key),
                "{key} must not be written: {encoded}"
            );
        }
    }

    #[test]
    fn a_file_without_a_schema_version_is_migrated() {
        let value = json!({
            "window": { "maximized": true },
            "last_project": "/projects/roadster"
        });

        let data = AppStateData::from_json_value(value).expect("migrate");

        assert_eq!(data.schema_version, STATE_SCHEMA_VERSION);
        assert!(data.window.maximized);
        assert_eq!(data.last_project, Some(PathBuf::from("/projects/roadster")));
        assert_eq!(
            data.layout.primary_sidebar_width,
            ProjectLayout::DEFAULT_PRIMARY_SIDEBAR_WIDTH
        );
        assert_eq!(
            data.layout.secondary_sidebar_width,
            ProjectLayout::DEFAULT_SECONDARY_SIDEBAR_WIDTH
        );
        // Panels are open by default; a file that predates them must not open a
        // window with everything hidden.
        assert!(data.layout.primary_sidebar_visible);
        assert!(data.layout.secondary_sidebar_visible);
        assert!(data.layout.bottom_panel_visible);
        assert!(
            !data.layout.table_low_at_top,
            "the chart orientation is the default for files that predate the toggle"
        );
    }

    #[test]
    fn a_newer_schema_is_rejected() {
        let value = json!({ "schema_version": STATE_SCHEMA_VERSION + 1 });
        assert!(AppStateData::from_json_value(value).is_err());
    }

    #[test]
    fn layout_sizes_are_clamped_on_read() {
        let layout = LayoutState {
            primary_sidebar_width: 5.,
            secondary_sidebar_width: 10_000.,
            bottom_panel_height: 50.,
            ..LayoutState::default()
        };

        assert_eq!(
            layout.primary_sidebar(),
            px(PRIMARY_SIDEBAR_WIDTH_RANGE.start)
        );
        assert_eq!(
            layout.secondary_sidebar(),
            px(SECONDARY_SIDEBAR_WIDTH_RANGE.end)
        );
        assert_eq!(layout.bottom_panel(), px(BOTTOM_PANEL_HEIGHT_RANGE.start));
    }

    /// Dragging a layout handle is the one path that panicked in the field, so
    /// the mapping from reported sizes to state is covered directly.
    #[test]
    fn dragging_the_primary_handle_records_the_navigation_width() {
        let mut layout = LayoutState::default();

        layout.apply_primary_sidebar(&[px(260.), px(800.)]);

        assert_eq!(layout.primary_sidebar_width, 260.);
        // The editor absorbs the rest; it is never stored.
        assert_eq!(
            layout.bottom_panel_height,
            ProjectLayout::DEFAULT_BOTTOM_PANEL_HEIGHT
        );
    }

    #[test]
    fn dragging_the_secondary_handle_records_the_inspector_width() {
        let mut layout = LayoutState::default();

        layout.apply_secondary_sidebar(&[px(900.), px(320.)]);

        assert_eq!(layout.secondary_sidebar_width, 320.);
        assert_eq!(
            layout.primary_sidebar_width,
            ProjectLayout::DEFAULT_PRIMARY_SIDEBAR_WIDTH
        );
    }

    #[test]
    fn size_mapping_tolerates_an_empty_or_short_list() {
        let mut layout = LayoutState::default();
        layout.apply_primary_sidebar(&[]);
        layout.apply_secondary_sidebar(&[px(900.)]);
        layout.apply_bottom_panel(&[px(600.)]);
        assert_eq!(layout, LayoutState::default());
    }

    #[test]
    fn dragging_the_bottom_handle_records_the_panel_height() {
        let mut layout = LayoutState::default();

        layout.apply_bottom_panel(&[px(600.), px(240.)]);
        assert_eq!(layout.bottom_panel_height, 240.);
    }

    /// The reference window helper cannot be built without a platform, so the
    /// stored shape is what these tests pin: flags survive a round trip, and a
    /// maximize flag is never inferred from a rectangle.
    #[test]
    fn a_maximized_window_keeps_its_restore_rectangle() {
        let mut data = AppStateData::default();
        data.window.maximized = true;
        data.window.bounds = Some(WindowRect {
            x: 120.,
            y: 80.,
            width: 1100.,
            height: 760.,
        });

        let encoded = serde_json::to_string(&data).expect("serialize");
        let decoded: AppStateData = serde_json::from_str(&encoded).expect("deserialize");

        assert!(decoded.window.maximized);
        assert_eq!(
            decoded.window.bounds.map(|rect| rect.width),
            Some(1100.),
            "the windowed size is what un-maximizing returns to"
        );
    }

    #[test]
    fn a_saved_rectangle_round_trips_through_bounds() {
        let rect = WindowRect {
            x: 120.,
            y: 80.,
            width: 1440.,
            height: 900.,
        };

        assert_eq!(WindowRect::from_bounds(&rect.to_bounds()), rect);
    }

    /// The stored rectangle is restored untouched — never shrunk to the screen.
    /// Resizing it would change the window size the platform compares against
    /// when it answers "is this window maximized", which is what decides whether
    /// a session reopens maximized.
    #[test]
    fn a_screen_sized_rectangle_keeps_its_size() {
        let rect = WindowRect {
            x: 0.,
            y: 39.,
            width: 1793.,
            height: 1123.,
        };

        let restored = rect.to_bounds();

        assert_eq!(restored.size.width, px(1793.));
        assert_eq!(restored.size.height, px(1123.));
        assert_eq!(restored.origin, point(px(0.), px(39.)));
    }

    /// A windowed rectangle the size of the display is a maximize artifact, and
    /// storing it is what made a maximized session reopen smaller: creating the
    /// window at that size and then zooming produces a *smaller* frame instead of
    /// a larger one.
    ///
    /// Measured on an 1800×1130 usable area (macOS 27, 3024×1964 @2x):
    /// `zoom:` fills to 1800×1130, and zooming a window that already fills it
    /// gives 1780×1110 — the "standard frame", 20 pt in from the trailing edges.
    /// Both are maximize-shaped; a 1700×1000 window is not.
    #[test]
    fn a_maximize_shaped_rectangle_is_never_stored() {
        let display = Bounds::new(point(px(0.), px(39.)), size(px(1800.), px(1130.)));

        // The rounding a zoomed frame can carry still counts.
        assert!(fills_display(
            &display,
            &Bounds::new(point(px(1.), px(39.)), size(px(1799.), px(1130.)))
        ));
        assert!(fills_display(&display, &display));

        // A frame recorded while the Dock was hidden is taller than the usable
        // area now; it is still maximize-shaped.
        assert!(fills_display(
            &display,
            &Bounds::new(point(px(0.), px(39.)), size(px(1800.), px(1169.)))
        ));

        // AppKit's "standard frame", the one seen in `state.json` and reported as
        // "it comes back smaller every time".
        assert!(fills_display(
            &display,
            &Bounds::new(point(px(0.), px(39.)), size(px(1780.), px(1110.)))
        ));

        // A genuinely windowed rectangle is kept, even a large one.
        assert!(!fills_display(
            &display,
            &Bounds::new(point(px(300.), px(200.)), size(px(1200.), px(800.)))
        ));
        assert!(!fills_display(
            &display,
            &Bounds::new(point(px(50.), px(60.)), size(px(1700.), px(1000.)))
        ));
    }

    /// Detecting a maximize is a different question from storing one: only the
    /// exact zoom frame counts, or an oversized window would read as maximized.
    ///
    /// macOS-only, like the helper it covers: Windows and Linux take the answer
    /// from the platform, so there is no zoom frame to recognise there.
    #[cfg(target_os = "macos")]
    #[test]
    fn only_the_exact_zoom_frame_reads_as_maximized() {
        let display = Bounds::new(point(px(0.), px(39.)), size(px(1800.), px(1130.)));

        assert!(is_zoom_frame(&display, &display));
        assert!(is_zoom_frame(
            &display,
            &Bounds::new(point(px(1.), px(39.)), size(px(1799.), px(1130.)))
        ));

        assert!(!is_zoom_frame(
            &display,
            &Bounds::new(point(px(0.), px(39.)), size(px(1800.), px(1169.)))
        ));
        assert!(!is_zoom_frame(
            &display,
            &Bounds::new(point(px(300.), px(200.)), size(px(1200.), px(800.)))
        ));
    }

    #[test]
    fn the_global_keeps_the_document_it_was_given() {
        let data = AppStateData::default();
        let global = AppState::with(data.clone());

        assert_eq!(global.data(), &data);
    }

    #[test]
    fn a_layout_round_trips_through_the_project_state() {
        let layout = LayoutState {
            primary_sidebar_width: 300.,
            secondary_sidebar_width: 260.,
            bottom_panel_height: 240.,
            primary_sidebar_visible: false,
            secondary_sidebar_visible: true,
            bottom_panel_visible: true,
            table_low_at_top: true,
        };

        let mut restored = LayoutState::default();
        restored.apply_project_layout(&layout.to_project_layout());

        assert_eq!(restored, layout);
    }
}
